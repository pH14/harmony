// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

type adapter struct {
	servers []string
	names   []string
}

func (adapter) Family() family.Family { return family.Register{} }

func (a adapter) Nodes() []string { return a.names }

func (a adapter) Connect(context.Context, int) (runner.Client, error) {
	return &client{servers: a.servers, names: a.names, http: newHTTP(), index: map[string]uint64{}}, nil
}

type client struct {
	servers []string
	names   []string
	http    *http.Client
	mu      sync.Mutex
	index   map[string]uint64
}

func newHTTP() *http.Client {
	return &http.Client{Transport: &http.Transport{
		Proxy:             nil,
		DialContext:       (&net.Dialer{Timeout: time.Second}).DialContext,
		DisableKeepAlives: true,
	}}
}

func (c *client) Close() {}

type entry struct {
	Key         string
	Value       []byte
	CreateIndex uint64
	ModifyIndex uint64
}

// Values are unique per operation, so the ModifyIndex Consul reports for a
// value names the one write that stored it.
func (c *client) remember(key, value string, index uint64) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.index[key+"\x00"+value] = index
}

func (c *client) known(key, value string) (uint64, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	index, ok := c.index[key+"\x00"+value]
	return index, ok
}

// Each operation goes to one server drawn from the SDK stream; a follower
// forwards writes and consistent reads to the leader.
func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	base := c.servers[sdk.Below(uint64(len(c.servers)))]
	switch op.F {
	case family.RegisterWrite:
		_, err := c.put(ctx, base, op.K, op.V.(string), "")
		return nil, err
	case family.RegisterCAS:
		pair := op.V.([]string)
		index, ok := c.known(op.K, pair[0])
		if !ok {
			current, err := c.get(ctx, base, op.K, "consistent")
			if err != nil {
				return nil, runner.Definite(err)
			}
			if current == nil || string(current.Value) != pair[0] {
				return nil, runner.Definite(errors.New("cas: current value differs"))
			}
			index = current.ModifyIndex
		}
		applied, err := c.put(ctx, base, op.K, pair[1], strconv.FormatUint(index, 10))
		if err != nil {
			return nil, err
		}
		if !applied {
			return nil, runner.Definite(errors.New("cas: modify index differs"))
		}
		return nil, nil
	case family.RegisterReadF:
		mode := "consistent"
		if op.Node != "" {
			pinned, err := c.fence(ctx, op.Node)
			if err != nil {
				return nil, runner.Definite(err)
			}
			base, mode = pinned, "stale"
		}
		current, err := c.get(ctx, base, op.K, mode)
		if err != nil {
			return nil, runner.Definite(err)
		}
		if current == nil {
			return nil, nil
		}
		c.remember(op.K, string(current.Value), current.ModifyIndex)
		return string(current.Value), nil
	case family.RegisterReplicas:
		copies, err := c.replicas(ctx, op.Node, op.V.(family.ReplicaQuery).Keys)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return copies, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

// A server applies committed entries in log order. Once a stale read on the
// pinned server returns the fence written after the read started, that
// server's store holds every write acknowledged before the read started. Each
// read writes its own fence key, so concurrent reads on one server cannot
// overwrite each other's fence.
func (c *client) fence(ctx context.Context, node string) (string, error) {
	at := slices.Index(c.names, node)
	if at < 0 {
		return "", fmt.Errorf("unknown node %q", node)
	}
	base := c.servers[at]
	value := fmt.Sprintf("%016x", sdk.Random())
	key := "fence-" + node + "-" + value
	applied, err := c.put(ctx, base, key, value, "")
	if err != nil {
		return "", fmt.Errorf("fence on %s: %w", node, err)
	}
	if !applied {
		return "", fmt.Errorf("fence on %s: put returned false", node)
	}
	for {
		current, err := c.get(ctx, base, key, "stale")
		if err == nil && current != nil && string(current.Value) == value {
			return base, nil
		}
		select {
		case <-ctx.Done():
			return "", fmt.Errorf("fence on %s: not applied: %w", node, ctx.Err())
		case <-time.After(10 * time.Millisecond):
		}
	}
}

func (c *client) replicas(ctx context.Context, node string, keys []string) (map[string]family.Copy, error) {
	base, err := c.fence(ctx, node)
	if err != nil {
		return nil, err
	}
	body, status, err := c.request(ctx, http.MethodGet, base+"/v1/kv/?recurse=true&stale", nil)
	if err != nil {
		return nil, err
	}
	copies := make(map[string]family.Copy, len(keys))
	for _, key := range keys {
		copies[key] = family.Copy{}
	}
	if status == http.StatusNotFound {
		return copies, nil
	}
	var list []entry
	if err := json.Unmarshal(body, &list); err != nil {
		return nil, fmt.Errorf("recurse: %w", err)
	}
	for _, e := range list {
		if _, wanted := copies[e.Key]; !wanted {
			continue
		}
		text := string(e.Value)
		copies[e.Key] = family.Copy{Value: &text, Meta: map[string]uint64{
			"create_index": e.CreateIndex,
			"modify_index": e.ModifyIndex,
		}}
	}
	return copies, nil
}

func (c *client) get(ctx context.Context, base, key, mode string) (*entry, error) {
	body, status, err := c.request(ctx, http.MethodGet, base+"/v1/kv/"+url.PathEscape(key)+"?"+mode, nil)
	if err != nil {
		return nil, err
	}
	if status == http.StatusNotFound {
		return nil, nil
	}
	var list []entry
	if err := json.Unmarshal(body, &list); err != nil {
		return nil, fmt.Errorf("get %s: %w", key, err)
	}
	if len(list) != 1 {
		return nil, fmt.Errorf("get %s: %d entries", key, len(list))
	}
	return &list[0], nil
}

func (c *client) put(ctx context.Context, base, key, value, cas string) (bool, error) {
	target := base + "/v1/kv/" + url.PathEscape(key)
	if cas != "" {
		target += "?cas=" + cas
	}
	body, status, err := c.request(ctx, http.MethodPut, target, []byte(value))
	if err != nil {
		return false, err
	}
	if status != http.StatusOK {
		return false, fmt.Errorf("put %s: status %d: %s", key, status, strings.TrimSpace(string(body)))
	}
	switch strings.TrimSpace(string(body)) {
	case "true":
		return true, nil
	case "false":
		return false, nil
	}
	return false, fmt.Errorf("put %s: unexpected body %q", key, body)
}

// Only a failed dial is definite. Every other error can follow a write that
// reached the Raft log.
func (c *client) request(ctx context.Context, method, target string, body []byte) ([]byte, int, error) {
	req, err := http.NewRequestWithContext(ctx, method, target, bytes.NewReader(body))
	if err != nil {
		return nil, 0, runner.Definite(err)
	}
	resp, err := c.http.Do(req)
	if err != nil {
		var opErr *net.OpError
		if errors.As(err, &opErr) && opErr.Op == "dial" {
			return nil, 0, runner.Definite(err)
		}
		return nil, 0, err
	}
	defer resp.Body.Close()
	text, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, 0, err
	}
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusNotFound {
		return nil, 0, fmt.Errorf("%s %s: status %d: %s", method, target, resp.StatusCode, strings.TrimSpace(string(text)))
	}
	return text, resp.StatusCode, nil
}

func (a adapter) Ready(ctx context.Context) error {
	c := &client{http: newHTTP(), index: map[string]uint64{}}
	return runner.AnyMember(ctx, a.servers, func(ctx context.Context, base string) error {
		applied, err := c.put(ctx, base, "ready", "1", "")
		if err == nil && applied {
			return nil
		}
		return fmt.Errorf("%s: applied=%v: %w", base, applied, err)
	})
}

func main() {
	servers := make([]string, 3)
	names := make([]string, 3)
	for i := range servers {
		servers[i] = fmt.Sprintf("http://10.77.0.%d:8500", i+1)
		names[i] = fmt.Sprintf("n%d", i+1)
	}
	runner.Main(adapter{servers: servers, names: names})
}
