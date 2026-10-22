// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"encoding/json"
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
	bucket            = "B"
	stream            = "KV_" + bucket
	keySubject        = "$KV." + bucket + "."
	replicas          = 3
	placementAttempts = 16
	msgGetSubject     = "$JS.API.STREAM.MSG.GET." + stream
	msgNotFound       = 10037
)

var servers = []string{
	"nats://10.77.0.1:4222",
	"nats://10.77.0.2:4222",
	"nats://10.77.0.3:4222",
}

var nodes = []string{"n1", "n2", "n3"}

type adapter struct{}

func (adapter) Family() family.Family { return family.Register{} }

func (adapter) Nodes() []string { return nodes }

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
		if op.Node != "" {
			value, err := c.readOn(ctx, kv, op.K, op.Node)
			if err != nil {
				return nil, runner.Definite(err)
			}
			return value, nil
		}
		value, revision, err := c.get(ctx, op.K)
		if err != nil {
			return nil, runner.Definite(err)
		}
		if value == nil {
			return nil, nil
		}
		c.remember(*value, revision)
		return *value, nil
	case family.RegisterCAS:
		pair := op.V.([]string)
		old, value := pair[0], pair[1]
		revision, ok := c.revision(old)
		if !ok {
			value, current, err := c.get(ctx, op.K)
			if err != nil {
				return nil, runner.Definite(err)
			}
			if value == nil || *value != old {
				return nil, runner.Definite(fmt.Errorf("key %s does not hold %q", op.K, old))
			}
			revision = current
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
	case family.RegisterReplicas:
		copies, err := c.replicasOn(ctx, kv, op.V.(family.ReplicaQuery).Keys, op.Node)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return copies, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

// The bucket handle picks direct or leader gets from the stream configuration
// it saw when it bound, and a direct get may come from any replica. An explicit
// message get request is answered only by the stream leader.
func (c *client) get(ctx context.Context, key string) (*string, uint64, error) {
	request, err := json.Marshal(map[string]string{"last_by_subj": keySubject + key})
	if err != nil {
		return nil, 0, err
	}
	reply, err := c.conn.RequestWithContext(ctx, msgGetSubject, request)
	if err != nil {
		return nil, 0, fmt.Errorf("get %s: %w", key, err)
	}
	var response struct {
		Message *struct {
			Sequence uint64 `json:"seq"`
			Header   []byte `json:"hdrs"`
			Data     []byte `json:"data"`
		} `json:"message"`
		Error *struct {
			ErrCode     int    `json:"err_code"`
			Description string `json:"description"`
		} `json:"error"`
	}
	if err := json.Unmarshal(reply.Data, &response); err != nil {
		return nil, 0, fmt.Errorf("get %s: %w", key, err)
	}
	if response.Error != nil {
		if response.Error.ErrCode == msgNotFound {
			return nil, 0, nil
		}
		return nil, 0, fmt.Errorf("get %s: %s", key, response.Error.Description)
	}
	if response.Message == nil {
		return nil, 0, fmt.Errorf("get %s: empty response", key)
	}
	if len(response.Message.Header) > 0 {
		header, err := nats.DecodeHeadersMsg(response.Message.Header)
		if err != nil {
			return nil, 0, fmt.Errorf("get %s: %w", key, err)
		}
		switch header.Get("KV-Operation") {
		case "DEL", "PURGE":
			return nil, 0, nil
		}
	}
	value := string(response.Message.Data)
	return &value, response.Message.Sequence, nil
}

// A one-replica consumer serves messages from the store of the server that
// hosts it. The read first waits for that store to hold a fence written after
// the read started, then takes the key's last message from it.
func (c *client) readOn(ctx context.Context, kv jetstream.KeyValue, key, node string) (any, error) {
	if err := c.fenceOn(ctx, kv, node); err != nil {
		return nil, err
	}
	last, err := c.consumerOn(ctx, node, jetstream.ConsumerConfig{
		FilterSubject: keySubject + key,
		DeliverPolicy: jetstream.DeliverLastPerSubjectPolicy,
	})
	if err != nil {
		return nil, err
	}
	defer c.dropConsumer(last)
	if last.CachedInfo().NumPending == 0 {
		return nil, nil
	}
	msg, err := next(ctx, last)
	if err != nil {
		return nil, fmt.Errorf("key %s on %s: %w", key, node, err)
	}
	switch msg.Headers().Get("KV-Operation") {
	case "DEL", "PURGE":
		return nil, nil
	}
	return string(msg.Data()), nil
}

func (c *client) fenceOn(ctx context.Context, kv jetstream.KeyValue, node string) error {
	fenceKey := "fence-" + node
	revision, err := kv.Put(ctx, fenceKey, []byte(fmt.Sprintf("%016x", sdk.Random())))
	if err != nil {
		return fmt.Errorf("fence: %w", err)
	}
	fence, err := c.consumerOn(ctx, node, jetstream.ConsumerConfig{
		FilterSubject: keySubject + fenceKey,
		DeliverPolicy: jetstream.DeliverByStartSequencePolicy,
		OptStartSeq:   revision,
	})
	if err != nil {
		return err
	}
	defer c.dropConsumer(fence)
	if _, err := next(ctx, fence); err != nil {
		return fmt.Errorf("fence %d on %s: %w", revision, node, err)
	}
	return nil
}

// The consumer delivers the last message of every subject in the bucket from
// the node's own store. The stream sequence of that message is the key's
// revision, which is the same on every replica.
func (c *client) replicasOn(ctx context.Context, kv jetstream.KeyValue, keys []string, node string) (map[string]family.Copy, error) {
	if err := c.fenceOn(ctx, kv, node); err != nil {
		return nil, err
	}
	last, err := c.consumerOn(ctx, node, jetstream.ConsumerConfig{
		FilterSubject: keySubject + ">",
		DeliverPolicy: jetstream.DeliverLastPerSubjectPolicy,
	})
	if err != nil {
		return nil, err
	}
	defer c.dropConsumer(last)
	copies := make(map[string]family.Copy, len(keys))
	for _, key := range keys {
		copies[key] = family.Copy{}
	}
	initial := last.CachedInfo().NumPending
	for received, pending := uint64(0), initial; received < initial && pending > 0; received++ {
		msg, err := next(ctx, last)
		if err != nil {
			return nil, fmt.Errorf("keys on %s: %w", node, err)
		}
		meta, err := msg.Metadata()
		if err != nil {
			return nil, fmt.Errorf("keys on %s: %w", node, err)
		}
		pending = meta.NumPending
		key := strings.TrimPrefix(msg.Subject(), keySubject)
		if _, wanted := copies[key]; !wanted {
			continue
		}
		switch msg.Headers().Get("KV-Operation") {
		case "DEL", "PURGE":
			copies[key] = family.Copy{}
			continue
		}
		value := string(msg.Data())
		copies[key] = family.Copy{Value: &value, Meta: map[string]uint64{"revision": meta.Sequence.Stream}}
	}
	return copies, nil
}

func (c *client) consumerOn(ctx context.Context, node string, config jetstream.ConsumerConfig) (jetstream.Consumer, error) {
	config.AckPolicy = jetstream.AckNonePolicy
	config.MemoryStorage = true
	config.Replicas = 1
	config.InactiveThreshold = 5 * time.Second
	var placed []string
	for attempt := 0; attempt < placementAttempts; attempt++ {
		consumer, err := c.js.CreateConsumer(ctx, stream, config)
		if err != nil {
			return nil, fmt.Errorf("consumer on %s: %w", node, err)
		}
		info := consumer.CachedInfo()
		if info.Cluster != nil && info.Cluster.Leader == node {
			return consumer, nil
		}
		c.dropConsumer(consumer)
		if info.Cluster != nil {
			placed = append(placed, info.Cluster.Leader)
		}
	}
	return nil, fmt.Errorf("no consumer placed on %s in %d attempts, placed on %v", node, placementAttempts, placed)
}

func (c *client) dropConsumer(consumer jetstream.Consumer) {
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		_ = c.js.DeleteConsumer(ctx, stream, consumer.CachedInfo().Name)
	}()
}

func next(ctx context.Context, consumer jetstream.Consumer) (jetstream.Msg, error) {
	for ctx.Err() == nil {
		fetchCtx, cancel := context.WithTimeout(ctx, time.Second)
		batch, err := consumer.Fetch(1, jetstream.FetchContext(fetchCtx))
		if err != nil {
			cancel()
			return nil, err
		}
		for msg := range batch.Messages() {
			cancel()
			return msg, nil
		}
		err = batch.Error()
		cancel()
		if err != nil && !errors.Is(err, nats.ErrTimeout) && !errors.Is(err, context.DeadlineExceeded) {
			return nil, err
		}
	}
	return nil, ctx.Err()
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
