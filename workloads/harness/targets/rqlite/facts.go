// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptrace"
	"net/url"
	"os"
	"slices"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"harmony.local/harness/kit/sdk"
)

const (
	factsEnv         = "HARNESS_RQLITE_FACTS"
	transferTimeouts = "/tmp/rqlite/stepdown-timeouts"
	statusTimeout    = time.Second
	anyTarget        = "any"
)

func factsOn() bool { return os.Getenv(factsEnv) == "1" }

func newStepdownFacts() *stepdownFacts {
	if !factsOn() {
		return nil
	}
	return &stepdownFacts{path: transferTimeouts, sample: leaderOf}
}

func newWriteFacts() *writeFacts {
	if !factsOn() {
		return nil
	}
	return &writeFacts{pending: map[int]bool{}}
}

func nodeAt(target string) int {
	parsed, err := url.Parse(target)
	if err != nil {
		return -1
	}
	port := parsed.Port()
	return slices.IndexFunc(nodes, func(base string) bool { return strings.HasSuffix(base, ":"+port) })
}

func nodeName(at int) string {
	if at < 0 || at >= len(nodeNames) {
		return "?"
	}
	return nodeNames[at]
}

// Raft reports its statistics as strings.
type statusTerm uint64

func (t *statusTerm) UnmarshalJSON(text []byte) error {
	if string(text) == "null" {
		return nil
	}
	value, err := strconv.ParseUint(strings.Trim(string(text), `"`), 10, 64)
	if err != nil {
		return fmt.Errorf("term %s: %w", text, err)
	}
	*t = statusTerm(value)
	return nil
}

type raftStatus struct {
	Store struct {
		Raft struct {
			State string     `json:"state"`
			Term  statusTerm `json:"term"`
		} `json:"raft"`
		Leader struct {
			NodeID string `json:"node_id"`
		} `json:"leader"`
	} `json:"store"`
}

func raftStatusOf(ctx context.Context, at int) (raftStatus, error) {
	ctx, cancel := context.WithTimeout(ctx, statusTimeout)
	defer cancel()
	c := newClient(nodes[at])
	defer c.Close()
	var status raftStatus
	err := c.status(ctx, &status)
	return status, err
}

// The leader's term comes from the leader itself: a follower's term can run
// ahead of the leader's while it campaigns.
func leaderOf(ctx context.Context, via int) (int, uint64) {
	status, err := raftStatusOf(ctx, via)
	if err != nil {
		return -1, 0
	}
	at := via
	if status.Store.Raft.State != "Leader" {
		at = slices.Index(nodeNames, status.Store.Leader.NodeID)
		if at < 0 || at == via {
			return -1, 0
		}
		if status, err = raftStatusOf(ctx, at); err != nil {
			return -1, 0
		}
	}
	if status.Store.Raft.State != "Leader" || status.Store.Raft.Term == 0 {
		return -1, 0
	}
	return at, uint64(status.Store.Raft.Term)
}

func (a stepdownAnswer) transferTimedOut() bool {
	if a.err != nil || a.status != http.StatusInternalServerError {
		return false
	}
	var body struct {
		Error string `json:"error"`
	}
	return a.text == stepdownTimedOut || json.Unmarshal([]byte(a.text), &body) == nil && body.Error == stepdownTimedOut
}

type transferTimeout struct {
	term   uint64
	target string
}

func readTimeouts(path string) ([]transferTimeout, error) {
	file, err := os.Open(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	defer file.Close()
	var timeouts []transferTimeout
	lines := bufio.NewScanner(file)
	for lines.Scan() {
		fields := strings.Fields(lines.Text())
		if len(fields) != 2 {
			return nil, fmt.Errorf("%s: line %q", path, lines.Text())
		}
		term, err := strconv.ParseUint(fields[0], 10, 64)
		if err != nil || term == 0 || !slices.Contains(nodeNames, fields[1]) {
			return nil, fmt.Errorf("%s: line %q", path, lines.Text())
		}
		timeouts = append(timeouts, transferTimeout{term: term, target: fields[1]})
	}
	return timeouts, lines.Err()
}

func keepTimeout(path string, timeout transferTimeout) error {
	out, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	_, err = fmt.Fprintf(out, "%d %s\n", timeout.term, timeout.target)
	return errors.Join(err, out.Close())
}

type stepdownFacts struct {
	via     int
	target  string
	path    string
	sample  func(context.Context, int) (int, uint64)
	leader  int
	term    uint64
	checked bool
	last    int
	err     error
}

func (f *stepdownFacts) details() map[string]any {
	return map[string]any{"via": nodeName(f.via), "target": f.target, "leader": nodeName(f.leader), "term": f.term}
}

func (f *stepdownFacts) before(ctx context.Context) {
	f.leader, f.term = f.sample(ctx, f.via)
	f.checked, f.last = false, -1
}

func (f *stepdownFacts) hop(target string) {
	f.last = nodeAt(target)
	if !f.checked && f.leader >= 0 && f.last == f.leader {
		f.check(true)
	}
}

func (f *stepdownFacts) check(toLeader bool) {
	f.checked = true
	again := false
	if toLeader && f.target != anyTarget {
		timeouts, err := readTimeouts(f.path)
		if err != nil {
			f.err = errors.Join(f.err, fmt.Errorf("timeouts history: %w", err))
		} else {
			again = slices.Contains(timeouts, transferTimeout{term: f.term, target: f.target})
		}
	}
	sdk.Sometimes(again, "second_transfer", f.details())
}

func (f *stepdownFacts) after(answer stepdownAnswer) {
	if !f.checked {
		f.check(false)
	}
	timedOut := answer.transferTimedOut()
	if timedOut && f.target != anyTarget && f.leader >= 0 && f.last == f.leader {
		if err := keepTimeout(f.path, transferTimeout{term: f.term, target: f.target}); err != nil {
			f.err = errors.Join(f.err, fmt.Errorf("keep a timeout: %w", err))
		}
	}
	sdk.Sometimes(timedOut, "transfer_timeout", f.details())
}

// The register facts read only the requests the clients already send, so the
// drive sends what it sends without them.
type writeFacts struct {
	mu      sync.Mutex
	pending map[int]bool
}

var writes *writeFacts

type writeAttempt struct {
	node    int
	settled bool
}

func (c *client) write(ctx context.Context, statements [][]any) (result, error) {
	if writes == nil {
		return c.execute(ctx, statements)
	}
	c.attempt = &writeAttempt{node: -1}
	defer func() {
		writes.ended(c.attempt)
		c.attempt = nil
	}()
	return c.execute(ctx, statements)
}

func (c *client) do(req *http.Request, attempt *writeAttempt) (*http.Response, error) {
	if writes == nil {
		return c.http.Do(req)
	}
	var sent atomic.Bool
	trace := &httptrace.ClientTrace{WroteRequest: func(info httptrace.WroteRequestInfo) { sent.Store(info.Err == nil) }}
	resp, err := c.http.Do(req.WithContext(httptrace.WithClientTrace(req.Context(), trace)))
	writes.reached(nodeAt(req.URL.String()), sent.Load(), err == nil, attempt)
	return resp, err
}

func (w *writeFacts) reached(node int, sent, answered bool, attempt *writeAttempt) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if (sent || answered) && w.pending[node] {
		sdk.Sometimes(answered, "first_node_answers", map[string]any{"node": nodeName(node)})
		if answered {
			delete(w.pending, node)
		}
	}
	if attempt == nil || !sent && !answered {
		return
	}
	attempt.node = node
	if answered {
		return
	}
	attempt.settled = true
	sdk.Sometimes(true, "write_unanswered", map[string]any{"node": nodeName(node)})
	if node >= 0 {
		w.pending[node] = true
	}
}

func (w *writeFacts) ended(attempt *writeAttempt) {
	w.mu.Lock()
	defer w.mu.Unlock()
	if !attempt.settled {
		sdk.Sometimes(false, "write_unanswered", map[string]any{"node": nodeName(attempt.node)})
	}
}

const (
	connectionsEnv    = "HARNESS_RQLITE_CONNECTIONS"
	clientConnections = "/tmp/rqlite/connections"
)

type connectionLog struct {
	mu   sync.Mutex
	path string
}

var connections *connectionLog

func newConnectionLog() *connectionLog {
	if os.Getenv(connectionsEnv) != "1" {
		return nil
	}
	return &connectionLog{path: clientConnections}
}

func (l *connectionLog) record(process, at int) error {
	l.mu.Lock()
	defer l.mu.Unlock()
	out, err := os.OpenFile(l.path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	_, err = fmt.Fprintf(out, "%d %s\n", process, nodeName(at))
	return errors.Join(err, out.Close())
}

// A process keeps its connection until an operation is abandoned, so the last
// line for each process names the node it sends to now.
func readConnections(path string) (map[int]string, error) {
	file, err := os.Open(path)
	if errors.Is(err, os.ErrNotExist) {
		return map[int]string{}, nil
	}
	if err != nil {
		return nil, err
	}
	defer file.Close()
	current := map[int]string{}
	lines := bufio.NewScanner(file)
	for lines.Scan() {
		fields := strings.Fields(lines.Text())
		if len(fields) != 2 {
			return nil, fmt.Errorf("%s: line %q", path, lines.Text())
		}
		process, err := strconv.Atoi(fields[0])
		if err != nil || !slices.Contains(nodeNames, fields[1]) {
			return nil, fmt.Errorf("%s: line %q", path, lines.Text())
		}
		current[process] = fields[1]
	}
	if err := lines.Err(); err != nil {
		return nil, err
	}
	return current, nil
}

type raftView struct {
	answered bool
	state    string
	leader   string
	term     uint64
	commit   uint64
	last     uint64
	snapshot uint64
}

func raftViewOf(ctx context.Context, base string) raftView {
	ctx, cancel := context.WithTimeout(ctx, statusTimeout)
	defer cancel()
	c := newClient(base)
	defer c.Close()
	var status struct {
		Store struct {
			Raft struct {
				State    string    `json:"state"`
				Term     *raftStat `json:"term"`
				Commit   *raftStat `json:"commit_index"`
				Last     *raftStat `json:"last_log_index"`
				Snapshot *raftStat `json:"last_snapshot_index"`
			} `json:"raft"`
			Leader struct {
				NodeID string `json:"node_id"`
			} `json:"leader"`
		} `json:"store"`
	}
	if c.status(ctx, &status) != nil {
		return raftView{}
	}
	r := status.Store.Raft
	if r.State == "" || r.Term == nil || r.Commit == nil || r.Last == nil || r.Snapshot == nil {
		return raftView{}
	}
	return raftView{answered: true, state: r.State, leader: status.Store.Leader.NodeID, term: uint64(*r.Term),
		commit: uint64(*r.Commit), last: uint64(*r.Last), snapshot: uint64(*r.Snapshot)}
}

func (v raftView) staleTail() uint64 {
	if !v.answered || v.last <= v.commit {
		return 0
	}
	return v.last - v.commit
}

// The snapshot facts read every node's /status before and after the request,
// all at once, so a paused node delays the hook by one status deadline each
// time.
type snapshotFacts struct {
	sample      func(context.Context, string) raftView
	connections string
	bases       []string
	via         int
	before      []raftView
	err         error
}

func newSnapshotFacts() *snapshotFacts {
	if !factsOn() {
		return nil
	}
	return &snapshotFacts{sample: raftViewOf, connections: clientConnections}
}

func (f *snapshotFacts) views(ctx context.Context) []raftView {
	views := make([]raftView, len(f.bases))
	var wg sync.WaitGroup
	for i, base := range f.bases {
		wg.Add(1)
		go func() {
			defer wg.Done()
			views[i] = f.sample(ctx, base)
		}()
	}
	wg.Wait()
	return views
}

func leaderOtherThan(views []raftView, x int) int {
	at := -1
	for i, v := range views {
		if i != x && v.answered && v.state == "Leader" && (at < 0 || v.term > views[at].term) {
			at = i
		}
	}
	return at
}

func (f *snapshotFacts) details(views []raftView) map[string]any {
	x := views[f.via]
	details := map[string]any{"node": nodeName(f.via), "state": x.state, "term": x.term, "commit": x.commit, "last": x.last, "snapshot": x.snapshot}
	if y := leaderOtherThan(views, f.via); y >= 0 {
		details["leader"], details["leader_term"], details["leader_snapshot"] = nodeName(y), views[y].term, views[y].snapshot
	}
	return details
}

func (f *snapshotFacts) start(ctx context.Context, bases []string, via int, trailing uint64, set bool) {
	f.bases, f.via = bases, via
	f.before = f.views(ctx)
	x := f.before[via]
	y := leaderOtherThan(f.before, via)
	details := f.details(f.before)
	details["trailing_logs"] = trailing
	deposed := y >= 0 && x.answered && (x.term < f.before[y].term || x.leader == "")
	sdk.Sometimes(deposed, "snapshot_on_deposed_leader", details)
	sdk.Sometimes(x.staleTail() > 0, "stale_tail", details)
	sdk.Sometimes(set && x.staleTail() > 0 && trailing <= x.staleTail(), "trailing_within_tail", details)
	current, err := readConnections(f.connections)
	if err != nil {
		f.err = errors.Join(f.err, fmt.Errorf("client connections: %w", err))
	}
	clients := 0
	for _, node := range current {
		if node == nodeName(via) {
			clients++
		}
	}
	sdk.Sometimes(x.answered && clients > 0, "client_on_node", map[string]any{"node": nodeName(via), "clients": clients})
}

func (f *snapshotFacts) end(ctx context.Context) {
	after := f.views(ctx)
	x, was := after[f.via], f.before[f.via]
	y := leaderOtherThan(after, f.via)
	details := f.details(after)
	details["snapshot_before"] = was.snapshot
	sdk.Sometimes(x.answered && x.snapshot == x.commit && x.last > x.commit, "snapshot_at_commit_with_tail", details)
	below := y >= 0 && x.answered && after[y].snapshot < x.snapshot
	sdk.Sometimes(below, "leader_snapshot_below", details)
	compacted := below && x.state == "Follower" && was.answered && x.snapshot > was.snapshot
	sdk.Sometimes(compacted, "follower_compacted_ahead", details)
}
