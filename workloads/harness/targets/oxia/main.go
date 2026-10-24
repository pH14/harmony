// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/oxia-db/oxia/oxia"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const requestTimeout = 5 * time.Second

type adapter struct {
	servers []string
}

func (adapter) Family() family.Family { return family.Register{} }

func (a adapter) Connect(ctx context.Context, _ int) (runner.Client, error) {
	c := &client{servers: a.servers, version: map[string]int64{}}
	if err := c.dial(ctx); err != nil {
		return nil, err
	}
	return c, nil
}

// The Oxia client learns shard leaders from the one server it dials. A
// client whose server went away keeps stale assignments, so after any error
// that may come from the connection the client is replaced by one that dials
// a server drawn from the SDK stream.
type client struct {
	servers []string
	oxia    oxia.SyncClient
	mu      sync.Mutex
	version map[string]int64
}

func (c *client) dial(ctx context.Context) error {
	address := c.servers[sdk.Below(uint64(len(c.servers)))]
	type dialed struct {
		client oxia.SyncClient
		err    error
	}
	done := make(chan dialed, 1)
	go func() {
		client, err := oxia.NewSyncClient(address, oxia.WithRequestTimeout(requestTimeout))
		done <- dialed{client, err}
	}()
	select {
	case d := <-done:
		if d.err != nil {
			return fmt.Errorf("dial %s: %w", address, d.err)
		}
		c.oxia = d.client
		return nil
	case <-ctx.Done():
		go func() {
			if d := <-done; d.client != nil {
				_ = d.client.Close()
			}
		}()
		return fmt.Errorf("dial %s: %w", address, ctx.Err())
	}
}

func (c *client) Close() {
	if c.oxia != nil {
		_ = c.oxia.Close()
		c.oxia = nil
	}
}

func (c *client) drop() {
	if c.oxia != nil {
		old := c.oxia
		c.oxia = nil
		go func() { _ = old.Close() }()
	}
}

// Values are unique per operation, so the version Oxia reports for a value
// names the one write that stored it.
func (c *client) remember(key, value string, version int64) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.version[key+"\x00"+value] = version
}

func (c *client) known(key, value string) (int64, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	version, ok := c.version[key+"\x00"+value]
	return version, ok
}

func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	if c.oxia == nil {
		if err := c.dial(ctx); err != nil {
			return nil, runner.Definite(err)
		}
	}
	result, err := c.invoke(ctx, op)
	if err != nil && !errors.Is(err, oxia.ErrKeyNotFound) && !errors.Is(err, oxia.ErrUnexpectedVersionId) {
		c.drop()
	}
	return result, err
}

func (c *client) invoke(ctx context.Context, op family.Op) (any, error) {
	switch op.F {
	case family.RegisterWrite:
		value := op.V.(string)
		_, version, err := c.oxia.Put(ctx, op.K, []byte(value))
		if err != nil {
			return nil, err
		}
		c.remember(op.K, value, version.VersionId)
		return nil, nil
	case family.RegisterCAS:
		pair := op.V.([]string)
		expected, ok := c.known(op.K, pair[0])
		if !ok {
			current, version, err := c.get(ctx, op.K)
			if err != nil {
				return nil, runner.Definite(err)
			}
			if current == nil || *current != pair[0] {
				return nil, runner.Definite(errors.New("cas: current value differs"))
			}
			expected = version
		}
		_, version, err := c.oxia.Put(ctx, op.K, []byte(pair[1]), oxia.ExpectedVersionId(expected))
		if errors.Is(err, oxia.ErrUnexpectedVersionId) {
			return nil, runner.Definite(err)
		}
		if err != nil {
			return nil, err
		}
		c.remember(op.K, pair[1], version.VersionId)
		return nil, nil
	case family.RegisterReadF:
		current, _, err := c.get(ctx, op.K)
		if err != nil {
			return nil, runner.Definite(err)
		}
		if current == nil {
			return nil, nil
		}
		return *current, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

func (c *client) get(ctx context.Context, key string) (*string, int64, error) {
	_, value, version, err := c.oxia.Get(ctx, key)
	if errors.Is(err, oxia.ErrKeyNotFound) {
		return nil, 0, nil
	}
	if err != nil {
		return nil, 0, err
	}
	text := string(value)
	c.remember(key, text, version.VersionId)
	return &text, version.VersionId, nil
}

func (a adapter) Ready(ctx context.Context) error {
	var errs []error
	for _, address := range a.servers {
		c := &client{servers: []string{address}, version: map[string]int64{}}
		if err := c.dial(ctx); err != nil {
			errs = append(errs, err)
			continue
		}
		_, _, err := c.oxia.Put(ctx, "ready", []byte("1"))
		c.drop()
		if err == nil {
			return nil
		}
		errs = append(errs, fmt.Errorf("%s: %w", address, err))
	}
	return errors.Join(errs...)
}

func main() {
	servers := make([]string, 3)
	for i := range servers {
		servers[i] = fmt.Sprintf("10.77.0.%d:6648", i+1)
	}
	runner.Main(adapter{servers: servers})
}
