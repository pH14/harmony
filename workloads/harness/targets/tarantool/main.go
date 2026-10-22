// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"sync"
	"time"

	"github.com/tarantool/go-iproto"
	"github.com/tarantool/go-tarantool/v3"
	_ "github.com/tarantool/go-tarantool/v3/uuid"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

type adapter struct {
	addresses []string
	names     []string
}

func (adapter) Family() family.Family { return family.Register{} }

func (a adapter) Nodes() []string { return a.names }

func (a adapter) Connect(context.Context, int) (runner.Client, error) {
	return newClient(a), nil
}

type client struct {
	adapter
	mu    sync.Mutex
	conns map[string]*tarantool.Connection
}

func newClient(a adapter) *client {
	return &client{adapter: a, conns: map[string]*tarantool.Connection{}}
}

func (c *client) Close() {
	c.mu.Lock()
	defer c.mu.Unlock()
	for name, conn := range c.conns {
		conn.Close()
		delete(c.conns, name)
	}
}

func (c *client) conn(ctx context.Context, node string) (*tarantool.Connection, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if conn, ok := c.conns[node]; ok && !conn.ClosedNow() {
		return conn, nil
	}
	at := slices.Index(c.names, node)
	if at < 0 {
		return nil, fmt.Errorf("unknown node %q", node)
	}
	dialCtx, cancel := context.WithTimeout(ctx, time.Second)
	defer cancel()
	conn, err := tarantool.Connect(dialCtx, tarantool.NetDialer{Address: c.addresses[at]}, tarantool.Opts{})
	if err != nil {
		return nil, fmt.Errorf("connect %s: %w", node, err)
	}
	c.conns[node] = conn
	return conn, nil
}

func (c *client) forget(node string, conn *tarantool.Connection) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.conns[node] == conn {
		delete(c.conns, node)
		conn.Close()
	}
}

// A failed connect never sent the request. A request error that a server
// returned names whether the request took effect; a lost connection or a
// timeout does not.
func (c *client) call(ctx context.Context, node, function string, args ...any) ([]any, error) {
	conn, err := c.conn(ctx, node)
	if err != nil {
		return nil, runner.Definite(err)
	}
	if args == nil {
		args = []any{}
	}
	data, err := conn.Do(tarantool.NewCallRequest(function).Args(args).Context(ctx)).Get()
	if err != nil {
		var server tarantool.ServerError
		if !errors.As(err, &server) {
			c.forget(node, conn)
		}
		if errors.Is(err, tarantool.ErrConnectionNotReady) || errors.Is(err, tarantool.ErrRateLimited) {
			return nil, runner.Definite(fmt.Errorf("%s on %s: %w", function, node, err))
		}
		return nil, fmt.Errorf("%s on %s: %w", function, node, err)
	}
	return data, nil
}

func serverCode(err error) (iproto.Error, bool) {
	var server tarantool.ServerError
	if errors.As(err, &server) {
		return server.Code, true
	}
	return 0, false
}

// ER_READONLY and ER_TRANSACTION_CONFLICT abort the transaction before it
// reaches the WAL.
func notApplied(err error) bool {
	code, ok := serverCode(err)
	return ok && (code == iproto.ER_READONLY || code == iproto.ER_TRANSACTION_CONFLICT)
}

// A write on a follower fails with ER_READONLY before it has any effect, so
// the client asks that node for the leader's name and sends the write there
// once.
func (c *client) change(ctx context.Context, node, function string, args ...any) ([]any, error) {
	data, err := c.call(ctx, node, function, args...)
	if code, ok := serverCode(err); ok && code == iproto.ER_READONLY {
		leader, lerr := c.call(ctx, node, "harness_leader")
		if lerr == nil && len(leader) == 1 {
			if name, ok := leader[0].(string); ok && name != "" && name != node && slices.Contains(c.names, name) {
				data, err = c.call(ctx, name, function, args...)
			}
		}
	}
	if err != nil && notApplied(err) {
		return nil, runner.Definite(err)
	}
	return data, err
}

func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	node := c.names[sdk.Below(uint64(len(c.names)))]
	switch op.F {
	case family.RegisterWrite:
		_, err := c.change(ctx, node, "harness_write", op.K, op.V.(string))
		return nil, err
	case family.RegisterCAS:
		pair := op.V.([]string)
		data, err := c.change(ctx, node, "harness_cas", op.K, pair[0], pair[1])
		if err != nil {
			return nil, err
		}
		if len(data) != 1 {
			return nil, fmt.Errorf("cas: reply %v", data)
		}
		if applied, ok := data[0].(bool); !ok || !applied {
			return nil, runner.Definite(errors.New("cas: current value differs"))
		}
		return nil, nil
	case family.RegisterReadF:
		if op.Node != "" {
			copies, err := c.pinned(ctx, op.Node, []string{op.K})
			if err != nil {
				return nil, runner.Definite(err)
			}
			if copies[op.K].Value == nil {
				return nil, nil
			}
			return *copies[op.K].Value, nil
		}
		data, err := c.call(ctx, node, "harness_read", op.K)
		if err != nil {
			return nil, runner.Definite(err)
		}
		found, value, err := decodeRead(data)
		if err != nil {
			return nil, runner.Definite(err)
		}
		if !found {
			return nil, nil
		}
		return value, nil
	case family.RegisterReplicas:
		copies, err := c.pinned(ctx, op.Node, op.V.(family.ReplicaQuery).Keys)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return copies, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

func decodeRead(data []any) (bool, string, error) {
	if len(data) != 1 {
		return false, "", fmt.Errorf("read: reply %v", data)
	}
	row, ok := data[0].([]any)
	if !ok || len(row) == 0 {
		return false, "", fmt.Errorf("read: reply %v", data)
	}
	found, ok := row[0].(bool)
	if !ok {
		return false, "", fmt.Errorf("read: reply %v", data)
	}
	if !found {
		return false, "", nil
	}
	if len(row) != 2 {
		return false, "", fmt.Errorf("read: reply %v", data)
	}
	value, ok := row[1].(string)
	if !ok {
		return false, "", fmt.Errorf("read: reply %v", data)
	}
	return true, value, nil
}

// The fence write commits after the read starts. An instance applies
// confirmed transactions in log order, so once its read-confirmed copy holds
// the fence value, it holds every write acknowledged before the read started.
func (c *client) pinned(ctx context.Context, node string, keys []string) (map[string]family.Copy, error) {
	fence := "fence-" + node
	value := fmt.Sprintf("%016x", sdk.Random())
	if _, err := c.change(ctx, node, "harness_write", fence, value); err != nil {
		return nil, fmt.Errorf("fence on %s: %w", node, err)
	}
	query := append([]string{fence}, keys...)
	for {
		data, err := c.call(ctx, node, "harness_local", query)
		if err == nil {
			copies, err := decodeLocal(data)
			if err != nil {
				return nil, err
			}
			if got := copies[fence].Value; got != nil && *got == value {
				delete(copies, fence)
				return copies, nil
			}
		}
		select {
		case <-ctx.Done():
			return nil, fmt.Errorf("fence on %s: not applied: %w", node, ctx.Err())
		case <-time.After(10 * time.Millisecond):
		}
	}
}

func decodeLocal(data []any) (map[string]family.Copy, error) {
	if len(data) != 1 {
		return nil, fmt.Errorf("local: reply %v", data)
	}
	rows, ok := data[0].([]any)
	if !ok {
		return nil, fmt.Errorf("local: reply %v", data)
	}
	copies := make(map[string]family.Copy, len(rows))
	for _, item := range rows {
		row, ok := item.([]any)
		if !ok || len(row) < 2 {
			return nil, fmt.Errorf("local: row %v", item)
		}
		key, ok := row[0].(string)
		found, fok := row[1].(bool)
		if !ok || !fok {
			return nil, fmt.Errorf("local: row %v", item)
		}
		if !found {
			copies[key] = family.Copy{}
			continue
		}
		if len(row) != 3 {
			return nil, fmt.Errorf("local: row %v", item)
		}
		value, ok := row[2].(string)
		if !ok {
			return nil, fmt.Errorf("local: row %v", item)
		}
		copies[key] = family.Copy{Value: &value}
	}
	return copies, nil
}

func (a adapter) Ready(ctx context.Context) error {
	c := newClient(a)
	defer c.Close()
	var errs []error
	for _, node := range a.names {
		if _, err := c.change(ctx, node, "harness_write", "ready", "1"); err == nil {
			return nil
		} else {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

func main() {
	addresses := make([]string, 3)
	names := make([]string, 3)
	for i := range addresses {
		addresses[i] = fmt.Sprintf("10.77.0.%d:3301", i+1)
		names[i] = fmt.Sprintf("n%d", i+1)
	}
	runner.Main(adapter{addresses: addresses, names: names})
}
