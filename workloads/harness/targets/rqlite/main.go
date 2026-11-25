// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"hash/fnv"
	"io"
	"net"
	"net/http"
	"os"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const (
	upsertSQL = "INSERT INTO kv(k, v, n, h) VALUES(?, ?, 1, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v, n = n + 1, h = (h + excluded.h) % 4503599627370496"
	casSQL    = "UPDATE kv SET v = ?, n = n + 1, h = (h + ?) % 4503599627370496 WHERE k = ? AND v = ?"
	readSQL   = "SELECT v FROM kv WHERE k = ?"
	keysSQL   = "SELECT k, v, n, h FROM kv"
	digestMod = 1 << 52
	appendSQL = "INSERT INTO log(v) VALUES(?)"
	scanSQL   = "SELECT v FROM log ORDER BY id"
	fenceSQL  = "UPDATE ready SET n = n + 1 WHERE k = 1"
	readLevel = "linearizable"
	modeEnv   = "HARNESS_RQLITE_MODE"
)

var nodes = []string{
	"http://127.0.0.1:4001",
	"http://127.0.0.1:4003",
	"http://127.0.0.1:4005",
}

var nodeNames = []string{"n1", "n2", "n3"}

var stepdownTargets = []string{"", "n1", "n2", "n3"}

const (
	stepdownLog          = "/tmp/rqlite/stepdown.log"
	stepdownTimedOut     = "leadership transfer timeout"
	stepdownTimeout      = 10 * time.Second
	stepdownRetryChoices = 3
	stepdownWaitChoices  = 11
	stepdownWaitStep     = 100 * time.Millisecond
)

const (
	snapshotLog       = "/tmp/rqlite/snapshot.log"
	snapshotTimeout   = 10 * time.Second
	snapshotExponents = 10
)

var readySQL = [][]any{
	{"CREATE TABLE IF NOT EXISTS kv (k INTEGER PRIMARY KEY, v TEXT, n INTEGER NOT NULL DEFAULT 0, h INTEGER NOT NULL DEFAULT 0)"},
	{"CREATE TABLE IF NOT EXISTS log (id INTEGER PRIMARY KEY, v TEXT)"},
	{"CREATE TABLE IF NOT EXISTS ready (k INTEGER PRIMARY KEY, n INTEGER)"},
	{"INSERT INTO ready(k, n) VALUES(1, 1) ON CONFLICT(k) DO UPDATE SET n = n + 1"},
}

type adapter struct {
	forward bool
}

func (a adapter) Family() family.Family {
	if a.forward {
		return family.Log{}
	}
	return family.Register{}
}

type client struct {
	base    string
	http    *http.Client
	forward bool
	attempt *writeAttempt
	hop     func(target string)
}

type result struct {
	Values       [][]any `json:"values"`
	RowsAffected int64   `json:"rows_affected"`
	LastInsertID int64   `json:"last_insert_id"`
	Error        string  `json:"error"`
}

type response struct {
	Results   []result `json:"results"`
	Error     string   `json:"error"`
	RaftIndex uint64   `json:"raft_index"`
	text      string
}

func newClient(base string) *client {
	return &client{
		base: base,
		http: &http.Client{
			Transport: &http.Transport{
				Proxy:             nil,
				DialContext:       (&net.Dialer{Timeout: time.Second}).DialContext,
				DisableKeepAlives: true,
			},
			CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
		},
	}
}

func (adapter) Nodes() []string { return nodeNames }

func (a adapter) Connect(_ context.Context, process int) (runner.Client, error) {
	if a.forward {
		return forwardClient{}, nil
	}
	at := int(sdk.Below("rqlite.connect", uint64(len(nodes))))
	if connections != nil {
		if err := connections.record(process, at); err != nil {
			fmt.Fprintf(os.Stderr, "connection record: %v\n", err)
		}
	}
	return newClient(nodes[at]), nil
}

type forwardClient struct{}

func (forwardClient) Close() {}

func (forwardClient) Invoke(ctx context.Context, op family.Op) (any, error) {
	c := newClient(nodes[sdk.Below("rqlite.forward", uint64(len(nodes)))])
	c.forward = true
	defer c.Close()
	switch op.F {
	case family.LogAppend:
		return c.appendValue(ctx, op.V.(string))
	case family.LogRead:
		level := readLevel
		if op.Node != "" {
			pinned, err := pinnedClient(ctx, op.Node)
			if err != nil {
				return nil, runner.Definite(err)
			}
			defer pinned.Close()
			c, level = pinned, "none"
		}
		values, err := c.scan(ctx, level)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return values, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

// A read at level none answers from the node's own SQLite database. The read
// first waits until that database has applied a fence write made after the
// read started.
func pinnedClient(ctx context.Context, node string) (*client, error) {
	at := slices.Index(nodeNames, node)
	if at < 0 {
		return nil, fmt.Errorf("unknown node %q", node)
	}
	fence, err := writeFence(ctx)
	if err != nil {
		return nil, answers.err(node, err, time.Now())
	}
	c := newClient(nodes[at])
	for {
		status, err := answers.poll(ctx, c, node)
		if err == nil && status.applied >= fence {
			return c, nil
		}
		select {
		case <-ctx.Done():
			c.Close()
			return nil, answers.err(node, fmt.Errorf("waiting for fence %d: %w", fence, errors.Join(ctx.Err(), err)), time.Now())
		case <-time.After(20 * time.Millisecond):
		}
	}
}

// The fence's raft index is the leader's commit index once the write returns,
// so a node whose applied index is below it has entries still to apply.
func writeFence(ctx context.Context) (uint64, error) {
	via := sdk.Below("rqlite.fence", uint64(len(nodes)))
	leader := newClient(nodes[via])
	defer leader.Close()
	resp, status, err := leader.post(ctx, "/db/execute?raft_index", [][]any{{fenceSQL}})
	if err != nil {
		return 0, fmt.Errorf("fence write to %s: %w", nodeNames[via], err)
	}
	if status != http.StatusOK || resp.Error != "" || resp.RaftIndex == 0 {
		return 0, fmt.Errorf("fence write to %s: status %d: %s", nodeNames[via], status, resp.text)
	}
	return resp.RaftIndex, nil
}

func (adapter) Fence(ctx context.Context) (uint64, error) { return writeFence(ctx) }

func (adapter) Applied(ctx context.Context, node string) (uint64, error) {
	at := slices.Index(nodeNames, node)
	if at < 0 {
		return 0, fmt.Errorf("unknown node %q", node)
	}
	c := newClient(nodes[at])
	defer c.Close()
	status, err := answers.poll(ctx, c, node)
	return status.applied, err
}

const appliedNeverFalls = "rqlite: a node's applied index never decreases while its process runs"

func declareStatusProperties() {
	sdk.Declare(sdk.KindAlways, appliedNeverFalls)
}

// The check retries a pinned read in new attempts, and a failed /status
// attempt returns no index, so the last index a /status answer returned is kept
// across attempts and named in each failed attempt's error.
//
// The answers also keep each node's highest applied index per process, which
// /status names by its start time. Within one process a correct node applies
// entries in order and installs a snapshot only to move forward, so a lower
// applied index means the node went back to an older state. The snapshot index
// is not compared: hashicorp/raft records it both when a local snapshot finishes
// persisting and when a remote install completes, with no order between the two,
// so it can fall while the state does not. Answers to concurrent requests can
// arrive out of order, so an answer is compared only with the highest value
// among answers that arrived before its request was sent. An answer that leaves
// the applied index out, as /status does while the store is closed, is not
// compared.
type statusAnswers struct {
	mu      sync.Mutex
	clock   uint64
	pending map[uint64]bool
	last    map[string]statusAnswer
	high    map[lifetime]*high
	assert  func(bool, string, map[string]any)
	held    map[string]bool
	failed  map[failure]bool
}

type statusAnswer struct {
	applied uint64
	at      time.Time
}

type nodeStatus struct {
	applied    uint64
	started    string
	hasApplied bool
}

type lifetime struct {
	node    string
	started string
}

type rise struct {
	value    uint64
	received uint64
}

type high struct {
	rises []rise
}

func (h *high) before(sent uint64) (uint64, bool) {
	for i := len(h.rises) - 1; i >= 0; i-- {
		if h.rises[i].received < sent {
			return h.rises[i].value, true
		}
	}
	return 0, false
}

func (h *high) observe(value, received, oldest uint64) {
	if n := len(h.rises); n == 0 || value > h.rises[n-1].value {
		h.rises = append(h.rises, rise{value: value, received: received})
	}
	keep := 0
	for i, r := range h.rises {
		if r.received < oldest {
			keep = i
		}
	}
	h.rises = h.rises[keep:]
}

type failure struct {
	lifetime
	property string
}

func newStatusAnswers(assert func(bool, string, map[string]any)) *statusAnswers {
	return &statusAnswers{
		pending: map[uint64]bool{},
		last:    map[string]statusAnswer{},
		high:    map[lifetime]*high{},
		assert:  assert,
		held:    map[string]bool{},
		failed:  map[failure]bool{},
	}
}

var answers = newStatusAnswers(sdk.Always)

func (s *statusAnswers) poll(ctx context.Context, c *client, node string) (nodeStatus, error) {
	sent := s.tick()
	status, err := c.indexes(ctx)
	if err != nil {
		s.forget(sent)
		return status, err
	}
	s.observe(node, sent, status, time.Now())
	return status, nil
}

func (s *statusAnswers) tick() uint64 {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.clock++
	s.pending[s.clock] = true
	return s.clock
}

func (s *statusAnswers) forget(sent uint64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.pending, sent)
}

func (s *statusAnswers) oldest() uint64 {
	oldest := s.clock + 1
	for sent := range s.pending {
		oldest = min(oldest, sent)
	}
	return oldest
}

func (s *statusAnswers) observe(node string, sent uint64, status nodeStatus, now time.Time) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.pending, sent)
	s.clock++
	s.last[node] = statusAnswer{applied: status.applied, at: now}
	if status.started == "" {
		return
	}
	key := lifetime{node: node, started: status.started}
	h := s.high[key]
	if h == nil {
		h = &high{}
		s.high[key] = h
	}
	if status.hasApplied {
		s.check(key, appliedNeverFalls, h, status.applied, sent)
	}
}

func (s *statusAnswers) check(key lifetime, property string, h *high, value, sent uint64) {
	if highest, ok := h.before(sent); ok && value < highest {
		f := failure{lifetime: key, property: property}
		if !s.failed[f] {
			s.failed[f] = true
			s.assert(false, property, map[string]any{"node": key.node, "start_time": key.started, "highest": highest, "answer": value})
		}
	} else if !s.held[property] {
		s.held[property] = true
		s.assert(true, property, nil)
	}
	h.observe(value, s.clock, s.oldest())
}

func (s *statusAnswers) err(node string, attempt error, now time.Time) error {
	s.mu.Lock()
	answer, ok := s.last[node]
	s.mu.Unlock()
	state := node + " has no /status answer on record"
	if ok {
		state = fmt.Sprintf("%s last answered /status at index %d, %.2fs before this error", node, answer.applied, now.Sub(answer.at).Seconds())
	}
	return fmt.Errorf("%s; last attempt: %w", state, attempt)
}

func (c *client) status(ctx context.Context, into any) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, c.base+"/status", nil)
	if err != nil {
		return err
	}
	httpResp, err := c.do(req, nil)
	if err != nil {
		return err
	}
	defer httpResp.Body.Close()
	if httpResp.StatusCode != http.StatusOK {
		return fmt.Errorf("status: %s", httpResp.Status)
	}
	if err := json.NewDecoder(httpResp.Body).Decode(into); err != nil {
		return fmt.Errorf("status: %w", err)
	}
	return nil
}

func (c *client) indexes(ctx context.Context) (nodeStatus, error) {
	var status struct {
		Store struct {
			DBAppliedIndex *uint64 `json:"db_applied_index"`
		} `json:"store"`
		Node struct {
			StartTime string `json:"start_time"`
		} `json:"node"`
	}
	if err := c.status(ctx, &status); err != nil {
		return nodeStatus{}, err
	}
	result := nodeStatus{started: status.Node.StartTime}
	if applied := status.Store.DBAppliedIndex; applied != nil {
		result.applied, result.hasApplied = *applied, true
	}
	return result, nil
}

// rqlite sends each Raft stat as a number when it parses as one and as a
// string otherwise.
type raftStat uint64

func (r *raftStat) UnmarshalJSON(data []byte) error {
	value, err := strconv.ParseUint(strings.Trim(string(data), `"`), 10, 64)
	if err != nil {
		return fmt.Errorf("raft stat %s: %w", data, err)
	}
	*r = raftStat(value)
	return nil
}

func (c *client) appendValue(ctx context.Context, value string) (any, error) {
	resp, status, err := c.post(ctx, "/db/execute", [][]any{{appendSQL, value}})
	if err != nil {
		return nil, err
	}
	if status != http.StatusOK {
		err := fmt.Errorf("append: status %d: %s", status, resp.text)
		if refused(resp.text) {
			return nil, runner.Definite(err)
		}
		return nil, err
	}
	if resp.Error != "" {
		err := fmt.Errorf("append: %s", resp.Error)
		if refused(resp.Error) {
			return nil, runner.Definite(err)
		}
		return nil, err
	}
	if len(resp.Results) != 1 {
		return nil, fmt.Errorf("append: %d results", len(resp.Results))
	}
	res := resp.Results[0]
	if res.Error != "" {
		return nil, fmt.Errorf("append: %s", res.Error)
	}
	if res.LastInsertID <= 0 {
		return nil, fmt.Errorf("append: last_insert_id %d", res.LastInsertID)
	}
	return uint64(res.LastInsertID), nil
}

// These are the errors a client would read as "the write was not applied":
// no leader, or the node could not reach the leader to forward it. The log
// checker reports any such append that a later read contains.
func refused(text string) bool {
	return text == "leader not found" || text == "not leader" || strings.Contains(text, "dial tcp")
}

func (c *client) scan(ctx context.Context, level string) ([]string, error) {
	resp, status, err := c.post(ctx, "/db/query?level="+level, [][]any{{scanSQL}})
	if err != nil {
		return nil, err
	}
	if status != http.StatusOK {
		return nil, fmt.Errorf("scan: status %d: %s", status, resp.text)
	}
	if resp.Error != "" {
		return nil, fmt.Errorf("scan: %s", resp.Error)
	}
	if len(resp.Results) != 1 {
		return nil, fmt.Errorf("scan: %d results", len(resp.Results))
	}
	res := resp.Results[0]
	if res.Error != "" {
		return nil, fmt.Errorf("scan: %s", res.Error)
	}
	values := make([]string, 0, len(res.Values))
	for _, row := range res.Values {
		if len(row) != 1 {
			return nil, fmt.Errorf("scan: row has %d columns", len(row))
		}
		value, ok := row[0].(string)
		if !ok {
			return nil, fmt.Errorf("scan: value %v has type %T", row[0], row[0])
		}
		values = append(values, value)
	}
	return values, nil
}

func (c *client) Close() { c.http.CloseIdleConnections() }

func key(k string) (int64, error) {
	return strconv.ParseInt(strings.TrimPrefix(k, "k"), 10, 64)
}

func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	if op.F == family.RegisterReplicas {
		pinned, err := pinnedClient(ctx, op.Node)
		if err != nil {
			return nil, runner.Definite(err)
		}
		defer pinned.Close()
		copies, err := pinned.replicas(ctx, op.V.(family.ReplicaQuery).Keys)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return copies, nil
	}
	k, err := key(op.K)
	if err != nil {
		return nil, runner.Definite(err)
	}
	switch op.F {
	case family.RegisterWrite:
		value := op.V.(string)
		_, err := c.write(ctx, [][]any{{upsertSQL, k, value, digest(value)}})
		return nil, err
	case family.RegisterCAS:
		pair := op.V.([]string)
		res, err := c.write(ctx, [][]any{{casSQL, pair[1], digest(pair[1]), k, pair[0]}})
		if err != nil {
			return nil, err
		}
		switch res.RowsAffected {
		case 0:
			return nil, runner.Definite(errors.New("cas: current value differs"))
		case 1:
			return nil, nil
		}
		return nil, fmt.Errorf("cas: rows_affected %d", res.RowsAffected)
	case family.RegisterReadF:
		reader, level := c, readLevel
		if op.Node != "" {
			pinned, err := pinnedClient(ctx, op.Node)
			if err != nil {
				return nil, runner.Definite(err)
			}
			defer pinned.Close()
			reader, level = pinned, "none"
		}
		value, err := reader.read(ctx, k, level)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return value, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

func (c *client) read(ctx context.Context, k int64, level string) (any, error) {
	resp, status, err := c.post(ctx, "/db/query?level="+level, [][]any{{readSQL, k}})
	if err != nil {
		return nil, err
	}
	if status != http.StatusOK {
		return nil, fmt.Errorf("query: status %d: %s", status, resp.text)
	}
	if resp.Error != "" {
		return nil, fmt.Errorf("query: %s", resp.Error)
	}
	if len(resp.Results) != 1 {
		return nil, fmt.Errorf("query: %d results", len(resp.Results))
	}
	res := resp.Results[0]
	if res.Error != "" {
		return nil, fmt.Errorf("query: %s", res.Error)
	}
	switch len(res.Values) {
	case 0:
		return nil, nil
	case 1:
		if len(res.Values[0]) != 1 {
			return nil, fmt.Errorf("query: row has %d columns", len(res.Values[0]))
		}
		switch value := res.Values[0][0].(type) {
		case nil:
			return nil, nil
		case string:
			return value, nil
		default:
			return nil, fmt.Errorf("query: value %v has type %T", value, value)
		}
	}
	return nil, fmt.Errorf("query: %d rows for one key", len(res.Values))
}

func digest(value string) int64 {
	hash := fnv.New64a()
	hash.Write([]byte(value))
	return int64(hash.Sum64() % digestMod)
}

// A row counts the writes applied to it and sums a hash of each written value,
// so a write one node applied alone still shows after the key is written again,
// even when a retry of that write evens the counts.
func (c *client) replicas(ctx context.Context, keys []string) (map[string]family.Copy, error) {
	resp, status, err := c.post(ctx, "/db/query?level=none", [][]any{{keysSQL}})
	if err != nil {
		return nil, err
	}
	if status != http.StatusOK {
		return nil, fmt.Errorf("query: status %d: %s", status, resp.text)
	}
	if resp.Error != "" {
		return nil, fmt.Errorf("query: %s", resp.Error)
	}
	if len(resp.Results) != 1 {
		return nil, fmt.Errorf("query: %d results", len(resp.Results))
	}
	res := resp.Results[0]
	if res.Error != "" {
		return nil, fmt.Errorf("query: %s", res.Error)
	}
	copies := make(map[string]family.Copy, len(keys))
	for _, name := range keys {
		copies[name] = family.Copy{}
	}
	for _, row := range res.Values {
		if len(row) != 4 {
			return nil, fmt.Errorf("query: row has %d columns", len(row))
		}
		k, ok := row[0].(float64)
		if !ok {
			return nil, fmt.Errorf("query: key %v has type %T", row[0], row[0])
		}
		name := fmt.Sprintf("k%d", int64(k))
		if _, wanted := copies[name]; !wanted {
			continue
		}
		writes, ok := row[2].(float64)
		if !ok {
			return nil, fmt.Errorf("query: write count %v has type %T", row[2], row[2])
		}
		sum, ok := row[3].(float64)
		if !ok {
			return nil, fmt.Errorf("query: write digest %v has type %T", row[3], row[3])
		}
		meta := map[string]uint64{"writes": uint64(writes), "digest": uint64(sum)}
		switch value := row[1].(type) {
		case nil:
			copies[name] = family.Copy{Meta: meta}
		case string:
			copies[name] = family.Copy{Value: &value, Meta: meta}
		default:
			return nil, fmt.Errorf("query: value %v has type %T", value, value)
		}
	}
	return copies, nil
}

func (c *client) execute(ctx context.Context, statements [][]any) (result, error) {
	results, err := c.executeAll(ctx, statements)
	if err != nil {
		return result{}, err
	}
	return results[0], nil
}

func (c *client) executeAll(ctx context.Context, statements [][]any) ([]result, error) {
	resp, status, err := c.post(ctx, "/db/execute", statements)
	if err != nil {
		return nil, err
	}
	switch {
	case status == http.StatusOK:
	case status == http.StatusInternalServerError && resp.text != "leader not found":
		return nil, fmt.Errorf("execute: status %d: %s", status, resp.text)
	default:
		return nil, runner.Definite(fmt.Errorf("execute: status %d: %s", status, resp.text))
	}
	if resp.Error == "not leader" || resp.Error == "store not open" {
		return nil, runner.Definite(fmt.Errorf("execute: %s", resp.Error))
	}
	if resp.Error != "" {
		return nil, fmt.Errorf("execute: %s", resp.Error)
	}
	if len(resp.Results) != len(statements) {
		return nil, fmt.Errorf("execute: %d results for %d statements", len(resp.Results), len(statements))
	}
	for _, res := range resp.Results {
		if res.Error != "" {
			return nil, runner.Definite(fmt.Errorf("execute: %s", res.Error))
		}
	}
	return resp.Results, nil
}

func (c *client) post(ctx context.Context, path string, statements [][]any) (response, int, error) {
	body, err := json.Marshal(statements)
	if err != nil {
		return response{}, 0, runner.Definite(err)
	}
	var status int
	var text string
	if c.forward {
		status, text, _, err = c.send(ctx, c.base+path, body)
	} else {
		status, text, err = c.postRedirect(ctx, path, body)
	}
	if err != nil {
		return response{}, 0, err
	}
	resp := response{text: strings.TrimSpace(text)}
	if status == http.StatusOK {
		if err := json.Unmarshal([]byte(text), &resp); err != nil {
			return response{}, 0, fmt.Errorf("decode %q: %w", text, err)
		}
	}
	return resp, status, nil
}

func (c *client) postRedirect(ctx context.Context, path string, body []byte) (int, string, error) {
	separator := "?"
	if strings.Contains(path, "?") {
		separator = "&"
	}
	target := c.base + path + separator + "redirect"
	for hop := 0; ; hop++ {
		if c.hop != nil {
			c.hop(target)
		}
		status, text, location, err := c.send(ctx, target, body)
		if err != nil || status != http.StatusMovedPermanently || hop > 0 || location == "" {
			return status, text, err
		}
		target = location
	}
}

func (c *client) send(ctx context.Context, target string, body []byte) (int, string, string, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, target, bytes.NewReader(body))
	if err != nil {
		return 0, "", "", runner.Definite(err)
	}
	req.Header.Set("Content-Type", "application/json")
	httpResp, err := c.do(req, c.attempt)
	if err != nil {
		var opErr *net.OpError
		if errors.As(err, &opErr) && opErr.Op == "dial" {
			return 0, "", "", runner.Definite(err)
		}
		return 0, "", "", err
	}
	defer httpResp.Body.Close()
	text, err := io.ReadAll(httpResp.Body)
	if err != nil {
		return 0, "", "", err
	}
	return httpResp.StatusCode, string(text), httpResp.Header.Get("Location"), nil
}

type stepdownAnswer struct {
	status int
	text   string
	err    error
}

func (a stepdownAnswer) timedOut() bool {
	return a.err == nil && a.status == http.StatusInternalServerError && strings.Contains(a.text, stepdownTimedOut)
}

func (a stepdownAnswer) String() string {
	if a.err != nil {
		return "error " + strconv.Quote(a.err.Error())
	}
	return fmt.Sprintf("status %d %s", a.status, strconv.Quote(a.text))
}

func (c *client) stepdown(ctx context.Context, id string) stepdownAnswer {
	ctx, cancel := context.WithTimeout(ctx, stepdownTimeout)
	defer cancel()
	var body []byte
	if id != "" {
		var err error
		body, err = json.Marshal(struct {
			ID string `json:"id"`
		}{id})
		if err != nil {
			return stepdownAnswer{err: err}
		}
	}
	status, text, err := c.postRedirect(ctx, "/leader?wait", body)
	return stepdownAnswer{status: status, text: strings.TrimSpace(text), err: err}
}

func (c *client) stepdownRetrying(ctx context.Context, id string, retries int, wait func() time.Duration, before func(), record func(stepdownAnswer)) {
	for attempt := 0; ; attempt++ {
		before()
		answer := c.stepdown(ctx, id)
		record(answer)
		if attempt == retries || !answer.timedOut() {
			return
		}
		time.Sleep(wait())
	}
}

func runStepdown() error {
	via := int(sdk.Below("rqlite.stepdown.via", uint64(len(nodes))))
	target := stepdownTargets[sdk.Below("rqlite.stepdown.target", uint64(len(stepdownTargets)))]
	retries := int(sdk.Below("rqlite.stepdown.retries", stepdownRetryChoices))
	out, err := os.OpenFile(stepdownLog, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	wait := func() time.Duration {
		return time.Duration(sdk.Below("rqlite.stepdown.wait", stepdownWaitChoices)) * stepdownWaitStep
	}
	return errors.Join(stepdownFrom(via, target, retries, wait, out, newStepdownFacts()), out.Close())
}

func runLeader() error {
	name, ok := leaderName(context.Background())
	if !ok {
		return errors.New("no node reports a leader")
	}
	_, err := fmt.Println(name)
	return err
}

func stepdownFrom(via int, target string, retries int, wait func() time.Duration, out io.Writer, facts *stepdownFacts) error {
	named := target
	if named == "" {
		named = anyTarget
	}
	c := newClient(nodes[via])
	defer c.Close()
	ctx := context.Background()
	before := func() {}
	if facts != nil {
		facts.via, facts.target = via, named
		before = func() { facts.before(ctx) }
		c.hop = facts.hop
	}
	var failed error
	c.stepdownRetrying(ctx, target, retries, wait, before, func(answer stepdownAnswer) {
		if facts != nil {
			facts.after(answer)
		}
		_, err := fmt.Fprintf(out, "%s via=%s target=%s retries=%d %s\n", uptime(), nodeNames[via], named, retries, answer)
		failed = errors.Join(failed, err)
	})
	if facts != nil {
		failed = errors.Join(failed, facts.err)
	}
	return failed
}

func uptime() string {
	text, err := os.ReadFile("/proc/uptime")
	if err != nil {
		return "uptime=?"
	}
	return "uptime=" + strings.Fields(string(text))[0]
}

func snapshotTrailing() (uint64, bool) {
	if sdk.Below("rqlite.snapshot.default", 4) == 0 {
		return 0, false
	}
	low := uint64(1) << sdk.Below("rqlite.snapshot.exponent", snapshotExponents)
	return low + sdk.Below("rqlite.snapshot.offset", low), true
}

func snapshotURL(base string, trailing uint64, set bool) string {
	if !set {
		return base + "/snapshot"
	}
	return base + "/snapshot?trailing_logs=" + strconv.FormatUint(trailing, 10)
}

func runSnapshot() error {
	out, err := os.OpenFile(snapshotLog, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	defer out.Close()
	return recordSnapshot(out, nodes, newSnapshotFacts())
}

func recordSnapshot(out io.Writer, bases []string, facts *snapshotFacts) error {
	via := sdk.Below("rqlite.snapshot.node", uint64(len(bases)))
	trailing, set := snapshotTrailing()
	named := "default"
	if set {
		named = strconv.FormatUint(trailing, 10)
	}
	ctx := context.Background()
	if facts != nil {
		facts.start(ctx, bases, int(via), trailing, set)
	}
	c := newClient(bases[via])
	defer c.Close()
	sent := uptime()
	answer := c.snapshot(ctx, trailing, set)
	_, err := fmt.Fprintf(out, "%s node=%s trailing_logs=%s %s\n", sent, nodeNames[via], named, answer)
	if facts != nil {
		facts.end(ctx)
		err = errors.Join(err, facts.err)
	}
	return err
}

func (c *client) snapshot(ctx context.Context, trailing uint64, set bool) string {
	ctx, cancel := context.WithTimeout(ctx, snapshotTimeout)
	defer cancel()
	status, text, _, err := c.send(ctx, snapshotURL(c.base, trailing, set), nil)
	if err != nil {
		return "error " + strconv.Quote(err.Error())
	}
	return fmt.Sprintf("status %d %s", status, strconv.Quote(strings.TrimSpace(text)))
}

func (adapter) Ready(ctx context.Context) error {
	return runner.AnyMember(ctx, nodes, func(ctx context.Context, base string) error {
		c := newClient(base)
		_, err := c.executeAll(ctx, readySQL)
		c.Close()
		if err != nil {
			return fmt.Errorf("%s: %w", base, err)
		}
		return nil
	})
}

func main() {
	if len(os.Args) == 2 {
		var run func() error
		switch os.Args[1] {
		case "stepdown":
			run = runStepdown
		case "snapshot":
			run = runSnapshot
		case "leader":
			run = runLeader
		}
		if run != nil {
			if err := run(); err != nil {
				fmt.Fprintln(os.Stderr, err)
				os.Exit(1)
			}
			return
		}
	}
	switch mode := os.Getenv(modeEnv); mode {
	case "":
		writes = newWriteFacts()
		connections = newConnectionLog()
		declareStatusProperties()
		runner.Main(adapter{})
	case "forward":
		if factsOn() {
			fmt.Fprintf(os.Stderr, "%s needs the register mode\n", factsEnv)
			os.Exit(2)
		}
		declareStatusProperties()
		runner.Main(adapter{forward: true})
	default:
		fmt.Fprintf(os.Stderr, "unknown %s %q\n", modeEnv, mode)
		os.Exit(2)
	}
}
