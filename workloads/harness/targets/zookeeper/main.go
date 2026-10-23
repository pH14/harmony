// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/go-zookeeper/zk"
	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const logPath = "/harmony-log"
const fencePath = "/harmony-fence"

type adapter struct{ servers []string }

func (adapter) Family() family.Family { return family.Log{} }
func (a adapter) Nodes() []string {
	names := make([]string, len(a.servers))
	for i := range names {
		names[i] = fmt.Sprintf("z%d", i+1)
	}
	return names
}

type client struct {
	servers []string
	conn    *zk.Conn
}

func (a adapter) Connect(ctx context.Context, process int) (runner.Client, error) {
	first := int(sdk.Below(uint64(len(a.servers))))
	servers := append(append([]string{}, a.servers[first:]...), a.servers[:first]...)
	c := &client{servers: servers}
	return c, c.connect(ctx)
}
func (c *client) connect(ctx context.Context) error {
	if c.conn != nil {
		return nil
	}
	conn, events, err := zk.Connect(c.servers, 10*time.Second)
	if err != nil {
		return err
	}
	for {
		select {
		case <-ctx.Done():
			conn.Close()
			return ctx.Err()
		case event, ok := <-events:
			if !ok {
				conn.Close()
				return errors.New("ZooKeeper event channel closed")
			}
			if event.State == zk.StateHasSession {
				c.conn = conn
				go func() {
					for range events {
					}
				}()
				return nil
			}
		}
	}
}
func (c *client) Close() {
	if c.conn != nil {
		c.conn.Close()
		c.conn = nil
	}
}
func (c *client) bounded(ctx context.Context, run func(*zk.Conn) (any, error)) (any, error) {
	if err := c.connect(ctx); err != nil {
		return nil, err
	}
	conn := c.conn
	closed := make(chan struct{})
	stop := context.AfterFunc(ctx, func() { conn.Close(); close(closed) })
	value, err := run(conn)
	if !stop() {
		<-closed
		c.conn = nil
	}
	if ctx.Err() != nil {
		return nil, ctx.Err()
	}
	return value, err
}
func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	if op.Node != "" {
		index, err := strconv.Atoi(strings.TrimPrefix(op.Node, "z"))
		if err != nil || index < 1 {
			return nil, runner.Definite(fmt.Errorf("bad node %q", op.Node))
		}
		// Use the node address, never a connection that can fail over elsewhere.
		address := fmt.Sprintf("10.77.0.%d:2181", index)
		if len(c.servers) == 1 && c.servers[0] == "127.0.0.1:2181" {
			address = c.servers[0]
		}
		pinned := &client{servers: []string{address}}
		defer pinned.Close()
		op.Node = ""
		return pinned.Invoke(ctx, op)
	}
	value, err := c.bounded(ctx, func(conn *zk.Conn) (any, error) {
		switch op.F {
		case family.LogAppend:
			path, err := conn.Create(logPath+"/entry-", []byte(op.V.(string)), zk.FlagSequence, zk.WorldACL(zk.PermAll))
			if err != nil {
				return nil, err
			}
			return strconv.ParseUint(strings.TrimPrefix(path, logPath+"/entry-"), 10, 64)
		case family.LogRead:
			// A successful write fences reads against a disconnected former leader.
			if _, err := conn.Set(fencePath, []byte("fence"), -1); err != nil {
				return nil, err
			}
			children, _, err := conn.Children(logPath)
			if err != nil {
				return nil, err
			}
			sort.Strings(children)
			values := make([]string, 0, len(children))
			for _, child := range children {
				data, _, err := conn.Get(logPath + "/" + child)
				if err != nil {
					return nil, err
				}
				values = append(values, string(data))
			}
			return values, nil
		default:
			return nil, fmt.Errorf("unknown operation %q", op.F)
		}
	})
	if op.F == family.LogRead {
		err = runner.Definite(err)
	}
	return value, err
}
func (a adapter) Ready(ctx context.Context) error {
	for _, address := range a.servers {
		c := &client{servers: []string{address}}
		_, err := c.bounded(ctx, func(conn *zk.Conn) (any, error) {
			for _, path := range []string{logPath, fencePath} {
				_, err := conn.Create(path, nil, 0, zk.WorldACL(zk.PermAll))
				if err != nil && !errors.Is(err, zk.ErrNodeExists) {
					return nil, err
				}
			}
			_, err := conn.Set(fencePath, []byte("ready"), -1)
			return nil, err
		})
		c.Close()
		if err != nil {
			return fmt.Errorf("%s: %w", address, err)
		}
	}
	fmt.Println("ZOOKEEPER_READY")
	return nil
}
func main() {
	count := 3
	if text := os.Getenv("ZOOKEEPER_MEMBERS"); text != "" {
		n, err := strconv.Atoi(text)
		if err != nil || (n != 1 && n != 3 && n != 5) {
			panic("ZOOKEEPER_MEMBERS must be 1, 3 or 5")
		}
		count = n
	}
	servers := make([]string, count)
	for i := range servers {
		servers[i] = fmt.Sprintf("10.77.0.%d:2181", i+1)
	}
	if count == 1 {
		servers[0] = "127.0.0.1:2181"
	}
	runner.Main(adapter{servers: servers})
}
