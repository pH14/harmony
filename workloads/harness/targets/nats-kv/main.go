// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const (
	bucket   = "B"
	stream   = "KV_" + bucket
	replicas = 3
)

var servers = []string{
	"nats://10.77.0.1:4222",
	"nats://10.77.0.2:4222",
	"nats://10.77.0.3:4222",
}

type adapter struct{}

func (adapter) Family() family.Family { return family.Register{} }

type client struct {
	conn      *nats.Conn
	js        jetstream.JetStream
	mu        sync.Mutex
	kv        jetstream.KeyValue
	revisions map[string]uint64
}

func dial(process int) (*client, error) {
	first := int(sdk.Below(uint64(len(servers))))
	ordered := append(append([]string(nil), servers[first:]...), servers[:first]...)
	conn, err := nats.Connect(strings.Join(ordered, ","),
		nats.Name(fmt.Sprintf("harness-kv-%d", process)),
		nats.DontRandomize(),
		nats.Timeout(time.Second),
		nats.MaxReconnects(-1),
		nats.ReconnectWait(100*time.Millisecond),
		nats.ReconnectJitter(0, 0),
		nats.ReconnectBufSize(-1),
		nats.PingInterval(time.Second),
		nats.MaxPingsOutstanding(3),
		nats.NoCallbacksAfterClientClose(),
	)
	if err != nil {
		return nil, err
	}
	js, err := jetstream.New(conn)
	if err != nil {
		conn.Close()
		return nil, err
	}
	return &client{conn: conn, js: js, revisions: map[string]uint64{}}, nil
}

func (adapter) Connect(_ context.Context, process int) (runner.Client, error) {
	return dial(process)
}

func (c *client) Close() { c.conn.Close() }

// The bucket handle caches the stream's AllowDirect setting when it binds, so
// it must bind after ready has turned direct gets off.
func (c *client) bucket(ctx context.Context) (jetstream.KeyValue, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.kv != nil {
		return c.kv, nil
	}
	kv, err := c.js.KeyValue(ctx, bucket)
	if err != nil {
		return nil, err
	}
	c.kv = kv
	return kv, nil
}

func (c *client) remember(value string, revision uint64) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.revisions[value] = revision
}

func (c *client) revision(value string) (uint64, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	revision, ok := c.revisions[value]
	return revision, ok
}

func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	kv, err := c.bucket(ctx)
	if err != nil {
		return nil, runner.Definite(err)
	}
	switch op.F {
	case family.RegisterWrite:
		value := op.V.(string)
		revision, err := kv.Put(ctx, op.K, []byte(value))
		if unsent(err) {
			return nil, runner.Definite(err)
		}
		if err != nil {
			return nil, err
		}
		c.remember(value, revision)
		return nil, nil
	case family.RegisterReadF:
		entry, err := kv.Get(ctx, op.K)
		if errors.Is(err, jetstream.ErrKeyNotFound) {
			return nil, nil
		}
		if err != nil {
			return nil, runner.Definite(err)
		}
		value := string(entry.Value())
		c.remember(value, entry.Revision())
		return value, nil
	case family.RegisterCAS:
		pair := op.V.([]string)
		old, value := pair[0], pair[1]
		revision, ok := c.revision(old)
		if !ok {
			entry, err := kv.Get(ctx, op.K)
			if err != nil {
				return nil, runner.Definite(err)
			}
			if string(entry.Value()) != old {
				return nil, runner.Definite(fmt.Errorf("key %s holds %q, not %q", op.K, entry.Value(), old))
			}
			revision = entry.Revision()
			c.remember(old, revision)
		}
		next, err := kv.Update(ctx, op.K, []byte(value), revision)
		if errors.Is(err, jetstream.ErrKeyRevisionMismatch) || unsent(err) {
			return nil, runner.Definite(err)
		}
		if err != nil {
			return nil, err
		}
		c.remember(value, next)
		return nil, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

// nats.go returns these from publish before the message is buffered, so the
// server never saw it.
func unsent(err error) bool {
	return errors.Is(err, nats.ErrReconnectBufExceeded) || errors.Is(err, nats.ErrConnectionDraining)
}

func (adapter) Ready(ctx context.Context) error {
	c, err := dial(0)
	if err != nil {
		return err
	}
	defer c.Close()
	if _, err := c.js.AccountInfo(ctx); err != nil {
		return err
	}
	if _, err := c.js.KeyValue(ctx, bucket); errors.Is(err, jetstream.ErrBucketNotFound) {
		_, err = c.js.CreateKeyValue(ctx, jetstream.KeyValueConfig{
			Bucket:   bucket,
			History:  1,
			Replicas: replicas,
			Storage:  jetstream.FileStorage,
		})
		if err != nil {
			return err
		}
	} else if err != nil {
		return err
	}
	s, err := c.js.Stream(ctx, stream)
	if err != nil {
		return err
	}
	info := s.CachedInfo()
	if info.Config.AllowDirect {
		config := info.Config
		config.AllowDirect = false
		if _, err := c.js.UpdateStream(ctx, config); err != nil {
			return err
		}
		return fmt.Errorf("turned direct gets off for %s", stream)
	}
	if info.Config.Replicas != replicas {
		return fmt.Errorf("stream %s has %d replicas", stream, info.Config.Replicas)
	}
	if info.Cluster == nil || info.Cluster.Leader == "" {
		return fmt.Errorf("stream %s has no leader", stream)
	}
	return nil
}

func main() {
	runner.Main(adapter{})
}
