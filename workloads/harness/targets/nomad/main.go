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

type variable struct {
	Path        string
	Items       map[string]string
	CreateIndex uint64
	ModifyIndex uint64
}

// Values are unique per operation, so the ModifyIndex Nomad reports for a
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
// forwards writes to the leader. Nomad has no consistent read mode, and its
// default mode lets a deposed leader answer from its own state, so every read
// is fenced through the server that answers it.
func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	at := int(sdk.Below(uint64(len(c.servers))))
	if op.Node != "" {
		at = slices.Index(c.names, op.Node)
		if at < 0 {
			return nil, runner.Definite(fmt.Errorf("unknown node %q", op.Node))
		}
	}
	base := c.servers[at]
	switch op.F {
	case family.RegisterWrite:
		_, err := c.put(ctx, base, op.K, op.V.(string), "")
		return nil, err
	case family.RegisterCAS:
		pair := op.V.([]string)
		index, ok := c.known(op.K, pair[0])
		if !ok {
			current, err := c.read(ctx, at, op.K)
			if err != nil {
				return nil, runner.Definite(err)
			}
			if current == nil || current.Items["v"] != pair[0] {
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
		current, err := c.read(ctx, at, op.K)
		if err != nil {
			return nil, runner.Definite(err)
		}
		if current == nil {
			return nil, nil
		}
		value := current.Items["v"]
		c.remember(op.K, value, current.ModifyIndex)
		return value, nil
	case family.RegisterReplicas:
		copies, err := c.replicas(ctx, at, op.V.(family.ReplicaQuery).Keys)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return copies, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

func (c *client) read(ctx context.Context, at int, key string) (*variable, error) {
	if err := c.fence(ctx, at); err != nil {
		return nil, err
	}
	return c.get(ctx, c.servers[at], key)
}

// A server applies committed entries in log order. Once a stale read on the
// server returns the fence written after the read started, that server's
// state holds every write acknowledged before the read started. Each read
// writes its own fence path, so concurrent reads on one server cannot
// overwrite each other's fence.
func (c *client) fence(ctx context.Context, at int) error {
	base, node := c.servers[at], c.names[at]
	value := fmt.Sprintf("%016x", sdk.Random())
	key := "fence-" + node + "-" + value
	applied, err := c.put(ctx, base, key, value, "")
	if err != nil {
		return fmt.Errorf("fence on %s: %w", node, err)
	}
	if !applied {
		return fmt.Errorf("fence on %s: put conflicted", node)
	}
	for {
		current, err := c.get(ctx, base, key)
		if err == nil && current != nil && current.Items["v"] == value {
			return nil
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("fence on %s: not applied: %w", node, ctx.Err())
		case <-time.After(10 * time.Millisecond):
		}
	}
}

func (c *client) replicas(ctx context.Context, at int, keys []string) (map[string]family.Copy, error) {
	if err := c.fence(ctx, at); err != nil {
		return nil, err
	}
	copies := make(map[string]family.Copy, len(keys))
	for _, key := range keys {
		current, err := c.get(ctx, c.servers[at], key)
		if err != nil {
			return nil, err
		}
		if current == nil {
			copies[key] = family.Copy{}
			continue
		}
		text := current.Items["v"]
		copies[key] = family.Copy{Value: &text, Meta: map[string]uint64{
			"create_index": current.CreateIndex,
			"modify_index": current.ModifyIndex,
		}}
	}
	return copies, nil
}

// A stale read answers from the receiving server's own state.
func (c *client) get(ctx context.Context, base, key string) (*variable, error) {
	body, status, err := c.request(ctx, http.MethodGet, base+"/v1/var/"+key+"?stale", nil)
	if err != nil {
		return nil, err
	}
	if status == http.StatusNotFound {
		return nil, nil
	}
	var v variable
	if err := json.Unmarshal(body, &v); err != nil {
		return nil, fmt.Errorf("get %s: %w", key, err)
	}
	if v.Path != key {
		return nil, fmt.Errorf("get %s: answered path %q", key, v.Path)
	}
	return &v, nil
}

// A cas Nomad rejects answers 409 Conflict with the stored variable.
func (c *client) put(ctx context.Context, base, key, value, cas string) (bool, error) {
	target := base + "/v1/var/" + key
	if cas != "" {
		target += "?cas=" + cas
	}
	payload, err := json.Marshal(map[string]map[string]string{"Items": {"v": value}})
	if err != nil {
		return false, runner.Definite(err)
	}
	body, status, err := c.request(ctx, http.MethodPut, target, payload)
	if err != nil {
		return false, err
	}
	switch status {
	case http.StatusOK:
		return true, nil
	case http.StatusConflict:
		return false, nil
	}
	return false, fmt.Errorf("put %s: status %d: %s", key, status, strings.TrimSpace(string(body)))
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
	switch resp.StatusCode {
	case http.StatusOK, http.StatusNotFound, http.StatusConflict:
		return text, resp.StatusCode, nil
	}
	return nil, 0, fmt.Errorf("%s %s: status %d: %s", method, target, resp.StatusCode, strings.TrimSpace(string(text)))
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
		servers[i] = fmt.Sprintf("http://10.77.0.%d:4646", i+1)
		names[i] = fmt.Sprintf("n%d", i+1)
	}
	runner.Main(adapter{servers: servers, names: names})
}
