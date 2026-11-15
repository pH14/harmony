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
	"sort"
	"strconv"
	"strings"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const schema = "k: string @index(exact) @upsert .\nv: string .\n"

type adapter struct {
	servers []string
	names   []string
}

func (adapter) Family() family.Family { return family.Register{} }

func (a adapter) Nodes() []string { return a.names }

func (a adapter) Connect(context.Context, int) (runner.Client, error) {
	return &client{servers: a.servers, names: a.names, http: newHTTP()}, nil
}

type client struct {
	servers []string
	names   []string
	http    *http.Client
}

func newHTTP() *http.Client {
	return &http.Client{Transport: &http.Transport{
		Proxy:             nil,
		DialContext:       (&net.Dialer{Timeout: time.Second}).DialContext,
		DisableKeepAlives: true,
	}}
}

func (c *client) Close() {}

type node struct {
	UID string  `json:"uid"`
	K   string  `json:"k"`
	V   *string `json:"v"`
}

type serverError struct{ messages []string }

func (e *serverError) Error() string { return strings.Join(e.messages, "; ") }

// Each operation goes to one Alpha drawn from the SDK stream. Any Alpha
// proposes a mutation to its group's leader and asks Zero for timestamps.
func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	base := c.servers[sdk.Below(uint64(len(c.servers)))]
	switch op.F {
	case family.RegisterWrite:
		_, err := c.mutate(ctx, base, writeUpsert(op.K, op.V.(string)))
		return nil, err
	case family.RegisterCAS:
		pair := op.V.([]string)
		matched, err := c.mutate(ctx, base, casUpsert(op.K, pair[0], pair[1]))
		if err != nil {
			return nil, err
		}
		if len(matched) != 1 {
			return nil, runner.Definite(fmt.Errorf("cas: %d nodes hold the old value", len(matched)))
		}
		return nil, nil
	case family.RegisterReadF:
		bestEffort := false
		if op.Node != "" {
			pinned, err := c.fence(ctx, op.Node)
			if err != nil {
				return nil, runner.Definite(err)
			}
			base, bestEffort = pinned, true
		}
		nodes, err := c.query(ctx, base, readQuery([]string{op.K}), bestEffort)
		if err != nil {
			return nil, runner.Definite(err)
		}
		held := merge(nodes)
		if held.Value == nil {
			return nil, nil
		}
		return *held.Value, nil
	case family.RegisterReplicas:
		copies, err := c.replicas(ctx, op.Node, op.V.(family.ReplicaQuery).Keys)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return copies, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

func quote(s string) string { return strconv.Quote(s) }

// An upsert whose query finds no node creates one. The @upsert index on k
// makes two transactions that create the same key conflict, so each key has
// at most one node.
func writeUpsert(key, value string) string {
	return fmt.Sprintf(`upsert {
  query { q(func: eq(k, %s)) { u as uid } }
  mutation { set {
    uid(u) <k> %s .
    uid(u) <v> %s .
  } }
}`, quote(key), quote(key), quote(value))
}

// The query and the conditional mutation run in one transaction, so the
// mutation applies exactly when the snapshot held the old value, and a
// concurrent write of the same node's v aborts one of the two.
func casUpsert(key, old, value string) string {
	return fmt.Sprintf(`upsert {
  query { q(func: eq(k, %s)) @filter(eq(v, %s)) { u as uid } }
  mutation @if(eq(len(u), 1)) { set {
    uid(u) <v> %s .
  } }
}`, quote(key), quote(old), quote(value))
}

func readQuery(keys []string) string {
	quoted := make([]string, len(keys))
	for i, key := range keys {
		quoted[i] = quote(key)
	}
	return fmt.Sprintf(`{ q(func: eq(k, [%s])) { uid k v } }`, strings.Join(quoted, ", "))
}

// Two nodes for one key break the upsert promise. Their values are joined
// into one that no write produced, so the checker reports the key.
func merge(nodes []node) family.Copy {
	var values []string
	var low uint64
	for _, n := range nodes {
		if n.V == nil {
			continue
		}
		values = append(values, *n.V)
		uid, _ := strconv.ParseUint(strings.TrimPrefix(n.UID, "0x"), 16, 64)
		if low == 0 || uid < low {
			low = uid
		}
	}
	if len(values) == 0 {
		return family.Copy{}
	}
	sort.Strings(values)
	text := strings.Join(values, "|")
	return family.Copy{Value: &text, Meta: map[string]uint64{"uid": low, "nodes": uint64(len(values))}}
}

// A best-effort query reads at the Alpha's own MaxAssigned timestamp and
// serves its group's predicates from its own store. Zero issues every commit
// timestamp, so a fence written after the read started commits above every
// write acknowledged before it. Once the Alpha's best-effort view holds the
// fence, its store holds those writes too. Each read writes its own fence key,
// so concurrent reads on one Alpha cannot satisfy each other's fence.
func (c *client) fence(ctx context.Context, name string) (string, error) {
	at := slices.Index(c.names, name)
	if at < 0 {
		return "", fmt.Errorf("unknown node %q", name)
	}
	base := c.servers[at]
	value := fmt.Sprintf("%016x", sdk.Random())
	key := "fence-" + name + "-" + value
	if _, err := c.mutate(ctx, base, fmt.Sprintf("{ set {\n_:f <k> %s .\n_:f <v> %s .\n} }", quote(key), quote(value))); err != nil {
		return "", fmt.Errorf("fence on %s: %w", name, err)
	}
	for {
		nodes, err := c.query(ctx, base, readQuery([]string{key}), true)
		if err == nil && len(nodes) == 1 && nodes[0].V != nil && *nodes[0].V == value {
			return base, nil
		}
		select {
		case <-ctx.Done():
			return "", fmt.Errorf("fence on %s: not applied: %w", name, ctx.Err())
		case <-time.After(10 * time.Millisecond):
		}
	}
}

func (c *client) replicas(ctx context.Context, name string, keys []string) (map[string]family.Copy, error) {
	base, err := c.fence(ctx, name)
	if err != nil {
		return nil, err
	}
	copies := make(map[string]family.Copy, len(keys))
	for _, key := range keys {
		copies[key] = family.Copy{}
	}
	if len(keys) == 0 {
		return copies, nil
	}
	nodes, err := c.query(ctx, base, readQuery(keys), true)
	if err != nil {
		return nil, err
	}
	byKey := map[string][]node{}
	for _, n := range nodes {
		byKey[n.K] = append(byKey[n.K], n)
	}
	for key := range copies {
		copies[key] = merge(byKey[key])
	}
	return copies, nil
}

func (c *client) query(ctx context.Context, base, dql string, bestEffort bool) ([]node, error) {
	target := base + "/query?ro=true"
	if bestEffort {
		target += "&be=true"
	}
	data, err := c.request(ctx, target, "application/dql", dql)
	if err != nil {
		return nil, err
	}
	var result struct {
		Q []node `json:"q"`
	}
	if err := json.Unmarshal(data, &result); err != nil {
		return nil, fmt.Errorf("query: %w", err)
	}
	return result.Q, nil
}

// A transaction that Zero aborts or that an Alpha refuses before it starts
// is a definite failure. Every other mutation error can follow a commit.
func (c *client) mutate(ctx context.Context, base, body string) ([]node, error) {
	data, err := c.request(ctx, base+"/mutate?commitNow=true", "application/rdf", body)
	var refused *serverError
	if errors.As(err, &refused) {
		text := refused.Error()
		if strings.Contains(text, "Transaction has been aborted") ||
			strings.Contains(text, "server is not ready to accept requests") {
			return nil, runner.Definite(err)
		}
	}
	if err != nil {
		return nil, err
	}
	var result struct {
		Queries struct {
			Q []node `json:"q"`
		} `json:"queries"`
	}
	if err := json.Unmarshal(data, &result); err != nil {
		return nil, fmt.Errorf("mutate: %w", err)
	}
	return result.Queries.Q, nil
}

// Only a failed dial is definite here. Dgraph answers errors with status 200
// and an errors list, which the caller classifies.
func (c *client) request(ctx context.Context, target, contentType, body string) (json.RawMessage, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, target, bytes.NewReader([]byte(body)))
	if err != nil {
		return nil, runner.Definite(err)
	}
	req.Header.Set("Content-Type", contentType)
	resp, err := c.http.Do(req)
	if err != nil {
		var opErr *net.OpError
		if errors.As(err, &opErr) && opErr.Op == "dial" {
			return nil, runner.Definite(err)
		}
		return nil, err
	}
	defer resp.Body.Close()
	text, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("%s: status %d: %s", target, resp.StatusCode, strings.TrimSpace(string(text)))
	}
	var envelope struct {
		Data   json.RawMessage `json:"data"`
		Errors []struct {
			Message string `json:"message"`
		} `json:"errors"`
	}
	if err := json.Unmarshal(text, &envelope); err != nil {
		return nil, fmt.Errorf("%s: %w: %s", target, err, strings.TrimSpace(string(text)))
	}
	if len(envelope.Errors) > 0 {
		e := &serverError{}
		for _, m := range envelope.Errors {
			e.messages = append(e.messages, m.Message)
		}
		return nil, e
	}
	return envelope.Data, nil
}

// An alter waits for every pending transaction, and a write that timed out
// stays pending until Zero aborts it, so ready alters only while the schema
// lacks the upsert index.
func (c *client) ensureSchema(ctx context.Context, base string) error {
	data, err := c.request(ctx, base+"/query?ro=true", "application/dql", "schema(pred: [k]) { index upsert }")
	if err != nil {
		return fmt.Errorf("schema: %w", err)
	}
	var result struct {
		Schema []struct {
			Predicate string `json:"predicate"`
			Index     bool   `json:"index"`
			Upsert    bool   `json:"upsert"`
		} `json:"schema"`
	}
	if err := json.Unmarshal(data, &result); err != nil {
		return fmt.Errorf("schema: %w", err)
	}
	for _, p := range result.Schema {
		if p.Predicate == "k" && p.Index && p.Upsert {
			return nil
		}
	}
	if _, err := c.request(ctx, base+"/alter", "application/dql", schema); err != nil {
		return fmt.Errorf("alter: %w", err)
	}
	return nil
}

func (a adapter) Ready(ctx context.Context) error {
	c := &client{http: newHTTP()}
	return runner.AnyMember(ctx, a.servers, func(ctx context.Context, base string) error {
		if err := c.ensureSchema(ctx, base); err != nil {
			return fmt.Errorf("%s: %w", base, err)
		}
		if _, err := c.mutate(ctx, base, writeUpsert("ready", "1")); err != nil {
			return fmt.Errorf("%s: write: %w", base, err)
		}
		return nil
	})
}

func main() {
	servers := make([]string, 3)
	names := make([]string, 3)
	for i := range servers {
		servers[i] = fmt.Sprintf("http://10.77.0.%d:8080", i+1)
		names[i] = fmt.Sprintf("n%d", i+1)
	}
	runner.Main(adapter{servers: servers, names: names})
}
