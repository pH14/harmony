// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/sdk"
)

type fact struct {
	id   string
	cond bool
	node string
	term uint64
}

func factSink(t *testing.T) func() []fact {
	t.Helper()
	dir := t.TempDir()
	t.Setenv("ANTITHESIS_OUTPUT_DIR", dir)
	return func() []fact {
		file, err := os.Open(filepath.Join(dir, "sdk.jsonl"))
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		if err != nil {
			t.Fatal(err)
		}
		defer file.Close()
		var facts []fact
		lines := bufio.NewScanner(file)
		for lines.Scan() {
			var record struct {
				Assert struct {
					ID        string `json:"id"`
					Condition bool   `json:"condition"`
					Type      string `json:"assert_type"`
					Details   struct {
						Node   string `json:"node"`
						Leader string `json:"leader"`
						Term   uint64 `json:"term"`
					} `json:"details"`
				} `json:"antithesis_assert"`
			}
			if err := json.Unmarshal(lines.Bytes(), &record); err != nil {
				t.Fatal(err)
			}
			if record.Assert.Type != "sometimes" {
				t.Fatalf("record %s is not a Sometimes", lines.Text())
			}
			details := record.Assert.Details
			facts = append(facts, fact{record.Assert.ID, record.Assert.Condition, details.Node + details.Leader, details.Term})
		}
		if err := lines.Err(); err != nil {
			t.Fatal(err)
		}
		return facts
	}
}

func expectFacts(t *testing.T, read func() []fact, want ...fact) {
	t.Helper()
	if got := read(); !slices.Equal(got, want) {
		t.Fatalf("facts %v, want %v", got, want)
	}
}

type fakeNode struct {
	*httptest.Server
	mu       sync.Mutex
	routes   map[string]http.HandlerFunc
	done     chan struct{}
	received chan struct{}
}

func fakeNodes(t *testing.T) []*fakeNode {
	t.Helper()
	saved := nodes
	done := make(chan struct{})
	var fakes []*fakeNode
	nodes = nil
	for range len(nodeNames) {
		f := &fakeNode{routes: map[string]http.HandlerFunc{}, done: done, received: make(chan struct{})}
		f.Server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			f.mu.Lock()
			route := f.routes[r.URL.Path]
			f.mu.Unlock()
			if route == nil {
				t.Errorf("unexpected %s %s", r.Method, r.URL)
				w.WriteHeader(http.StatusTeapot)
				return
			}
			route(w, r)
		}))
		t.Cleanup(f.Close)
		nodes = append(nodes, f.URL)
		fakes = append(fakes, f)
	}
	t.Cleanup(func() {
		nodes = saved
		close(done)
	})
	return fakes
}

func (f *fakeNode) handle(path string, route http.HandlerFunc) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.routes[path] = route
}

func (f *fakeNode) stall(headers bool) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, r.Body)
		if headers {
			w.WriteHeader(http.StatusInternalServerError)
			w.(http.Flusher).Flush()
		}
		select {
		case f.received <- struct{}{}:
		case <-r.Context().Done():
			return
		case <-f.done:
			return
		}
		select {
		case <-r.Context().Done():
		case <-f.done:
		}
	}
}

func hangUp(t *testing.T) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, r.Body)
		conn, _, err := w.(http.Hijacker).Hijack()
		if err != nil {
			t.Error(err)
			return
		}
		conn.Close()
	}
}

func raft(state, term, leader string) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		fmt.Fprintf(w, `{"store":{"db_applied_index":7,"raft":{"state":%q,"term":%q},"leader":{"node_id":%q}}}`, state, term, leader)
	}
}

func redirect(to *fakeNode, path string) http.HandlerFunc {
	return func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Location", to.URL+path)
		w.WriteHeader(http.StatusMovedPermanently)
	}
}

func inTurn(t *testing.T, answers ...func(http.ResponseWriter)) http.HandlerFunc {
	var mu sync.Mutex
	next := 0
	return func(w http.ResponseWriter, _ *http.Request) {
		mu.Lock()
		at := next
		next++
		mu.Unlock()
		if at >= len(answers) {
			t.Errorf("request %d past the last answer", at+1)
			w.WriteHeader(http.StatusTeapot)
			return
		}
		answers[at](w)
	}
}

func noWait() time.Duration { return 0 }

func hookRun(via int, target string, retries int, history string) (string, error) {
	var log bytes.Buffer
	err := stepdownFrom(via, target, retries, noWait, &log, &stepdownFacts{path: history, sample: leaderOf})
	return log.String(), err
}

func readHistory(t *testing.T, path string) string {
	t.Helper()
	text, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return ""
	}
	if err != nil {
		t.Fatal(err)
	}
	return string(text)
}

func TestIndexesReadOnlyTheIndexes(t *testing.T) {
	server, _ := leaderServer(t, answer(http.StatusOK, `{"store":{"db_applied_index":12,"raft":{"state":7,"term":null}}}`))
	c := newClient(server.URL)
	defer c.Close()
	if status, err := c.indexes(context.Background()); err != nil || status.applied != 12 {
		t.Fatalf("status %+v, error %v", status, err)
	}
}

func TestNodeAtNamesTheNodeByItsHTTPPort(t *testing.T) {
	for target, want := range map[string]int{
		"http://127.0.0.1:4001/db/execute?redirect": 0,
		"http://10.77.0.2:4003/db/execute?redirect": 1,
		"http://10.77.0.3:4005/leader?wait":         2,
		"http://127.0.0.1:4002/db/execute":          -1,
		"":                                          -1,
	} {
		if got := nodeAt(target); got != want {
			t.Errorf("nodeAt(%q) = %d, want %d", target, got, want)
		}
	}
}

func TestOnlyTheTimeoutErrorItselfIsATransferTimeout(t *testing.T) {
	for text, want := range map[string]bool{
		`{"error":"leadership transfer timeout"}`:          true,
		`{"error": "leadership transfer timeout"}`:         true,
		"leadership transfer timeout":                      true,
		`{"error":"leadership transfer timeout after 1s"}`: false,
		`{"error":"leadership transfer in progress"}`:      false,
		`{"error":"cannot transfer leadership to itself"}`: false,
	} {
		if got := (stepdownAnswer{status: http.StatusInternalServerError, text: text}).transferTimedOut(); got != want {
			t.Errorf("%s: %v, want %v", text, got, want)
		}
	}
	if (stepdownAnswer{status: http.StatusOK, text: "leadership transfer timeout"}).transferTimedOut() {
		t.Error("a 200 answer is a timeout")
	}
}

func TestALaterHookRunPassesSecondTransferInTheTermOfAnEarlierTimeout(t *testing.T) {
	read := factSink(t)
	n := fakeNodes(t)
	history := filepath.Join(t.TempDir(), "timeouts")
	n[0].handle("/status", raft("Follower", "15", "n2"))
	n[0].handle("/leader", redirect(n[1], "/leader?wait&redirect"))
	n[1].handle("/status", raft("Leader", "15", "n2"))
	n[1].handle("/leader", inTurn(t, timedOut, accepted))
	if log, err := hookRun(0, "n3", 0, history); err != nil || !strings.Contains(log, `via=n1 target=n3 retries=0 status 500`) {
		t.Fatalf("first run: error %v, log %q", err, log)
	}
	if _, err := hookRun(1, "n3", 0, history); err != nil {
		t.Fatalf("second run: %v", err)
	}
	expectFacts(t, read,
		fact{"second_transfer", false, "n2", 15}, fact{"transfer_timeout", true, "n2", 15},
		fact{"second_transfer", true, "n2", 15}, fact{"transfer_timeout", false, "n2", 15})
	if got := readHistory(t, history); got != "15 n3\n" {
		t.Fatalf("history %q", got)
	}
}

func TestARedirectAwayFromTheLeaderReadBeforeTheRequestKeepsNothing(t *testing.T) {
	read := factSink(t)
	n := fakeNodes(t)
	history := filepath.Join(t.TempDir(), "timeouts")
	if err := os.WriteFile(history, []byte("15 n1\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	n[0].handle("/status", raft("Follower", "15", "n2"))
	n[0].handle("/leader", redirect(n[2], "/leader?wait&redirect"))
	n[1].handle("/status", raft("Leader", "15", "n2"))
	n[2].handle("/leader", inTurn(t, timedOut))
	if _, err := hookRun(0, "n1", 0, history); err != nil {
		t.Fatal(err)
	}
	expectFacts(t, read, fact{"second_transfer", false, "n2", 15}, fact{"transfer_timeout", true, "n2", 15})
	if got := readHistory(t, history); got != "15 n1\n" {
		t.Fatalf("history %q", got)
	}
}

func TestATransferWithoutATargetOrALeaderKeepsNothing(t *testing.T) {
	read := factSink(t)
	n := fakeNodes(t)
	history := filepath.Join(t.TempDir(), "timeouts")
	n[0].handle("/status", raft("Leader", "15", "n1"))
	n[0].handle("/leader", inTurn(t, timedOut, timedOut))
	n[1].handle("/status", func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusServiceUnavailable) })
	n[1].handle("/leader", inTurn(t, timedOut))
	if _, err := hookRun(0, "", 1, history); err != nil {
		t.Fatal(err)
	}
	if _, err := hookRun(1, "n3", 0, history); err != nil {
		t.Fatal(err)
	}
	expectFacts(t, read,
		fact{"second_transfer", false, "n1", 15}, fact{"transfer_timeout", true, "n1", 15},
		fact{"second_transfer", false, "n1", 15}, fact{"transfer_timeout", true, "n1", 15},
		fact{"second_transfer", false, "?", 0}, fact{"transfer_timeout", true, "?", 0})
	if got := readHistory(t, history); got != "" {
		t.Fatalf("history %q", got)
	}
}

func TestASecondTransferIsCheckedBeforeTheHopToTheSampledLeader(t *testing.T) {
	read := factSink(t)
	n := fakeNodes(t)
	history := filepath.Join(t.TempDir(), "timeouts")
	if err := os.WriteFile(history, []byte("15 n3\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	n[0].handle("/status", raft("Leader", "15", "n1"))
	n[0].handle("/leader", redirect(n[1], "/leader?wait&redirect"))
	n[1].handle("/leader", inTurn(t, timedOut))
	if _, err := hookRun(0, "n3", 0, history); err != nil {
		t.Fatal(err)
	}
	expectFacts(t, read, fact{"second_transfer", true, "n1", 15}, fact{"transfer_timeout", true, "n1", 15})
	if got := readHistory(t, history); got != "15 n3\n" {
		t.Fatalf("history %q", got)
	}
}

func TestAnUnreadableHistoryFailsTheHookAndStillSendsTheRequest(t *testing.T) {
	for name, text := range map[string]string{
		"one field":     "15\n",
		"term zero":     "0 n2\n",
		"unknown node":  "15 n4\n",
		"no term":       "x n2\n",
		"line too long": strings.Repeat("x", 70000) + "\n",
	} {
		t.Run(name, func(t *testing.T) {
			read := factSink(t)
			n := fakeNodes(t)
			history := filepath.Join(t.TempDir(), "timeouts")
			if err := os.WriteFile(history, []byte("15 n2\n"+text), 0o644); err != nil {
				t.Fatal(err)
			}
			n[0].handle("/status", raft("Leader", "15", "n1"))
			n[0].handle("/leader", inTurn(t, accepted))
			log, err := hookRun(0, "n2", 0, history)
			if err == nil || !strings.Contains(err.Error(), "timeouts history") || !strings.Contains(log, "status 200") {
				t.Fatalf("error %v, log %q", err, log)
			}
			expectFacts(t, read, fact{"second_transfer", false, "n1", 15}, fact{"transfer_timeout", false, "n1", 15})
		})
	}
}

func TestATimeoutIsKeptBeforeTheHookLogsIt(t *testing.T) {
	factSink(t)
	n := fakeNodes(t)
	history := filepath.Join(t.TempDir(), "timeouts")
	n[0].handle("/status", raft("Leader", "15", "n1"))
	n[0].handle("/leader", inTurn(t, timedOut))
	var kept []string
	out := writerFunc(func(line []byte) (int, error) {
		kept = append(kept, readHistory(t, history))
		return len(line), nil
	})
	if err := stepdownFrom(0, "n2", 0, noWait, out, &stepdownFacts{path: history, sample: leaderOf}); err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(kept, []string{"15 n2\n"}) {
		t.Fatalf("history at the log line %q", kept)
	}
}

type writerFunc func([]byte) (int, error)

func (w writerFunc) Write(line []byte) (int, error) { return w(line) }

func TestTheVariableTurnsTheFactsOn(t *testing.T) {
	t.Setenv(factsEnv, "")
	if newStepdownFacts() != nil || newWriteFacts() != nil || newSnapshotFacts() != nil {
		t.Fatal("facts without the variable")
	}
	t.Setenv(factsEnv, "1")
	facts, w, s := newStepdownFacts(), newWriteFacts(), newSnapshotFacts()
	if facts == nil || facts.path != transferTimeouts || facts.sample == nil || w == nil || w.pending == nil || s == nil || s.sample == nil {
		t.Fatalf("stepdown facts %+v, write facts %+v, snapshot facts %+v", facts, w, s)
	}
}

func useWriteFacts(t *testing.T) {
	writes = &writeFacts{pending: map[int]bool{}}
	t.Cleanup(func() { writes = nil })
}

func within(t *testing.T, d time.Duration) context.Context {
	ctx, cancel := context.WithTimeout(context.Background(), d)
	t.Cleanup(cancel)
	return ctx
}

func untilReceived(t *testing.T, node *fakeNode) context.Context {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	t.Cleanup(cancel)
	go func() {
		select {
		case <-node.received:
		case <-ctx.Done():
		}
		cancel()
	}()
	return ctx
}

func writeWith(ctx context.Context, base string) error {
	c := newClient(base)
	defer c.Close()
	_, err := c.write(ctx, [][]any{{"UPDATE kv SET n = 1"}})
	return err
}

func indexWith(ctx context.Context, base string) error {
	c := newClient(base)
	defer c.Close()
	_, err := c.indexes(ctx)
	return err
}

func TestARedirectedWriteWithoutAReplyLeavesTheLeaderPendingUntilItAnswers(t *testing.T) {
	read := factSink(t)
	n := fakeNodes(t)
	useWriteFacts(t)
	n[0].handle("/db/execute", redirect(n[1], "/db/execute?redirect"))
	n[1].handle("/db/execute", n[1].stall(false))
	n[1].handle("/status", n[1].stall(false))
	if err := writeWith(untilReceived(t, n[1]), nodes[0]); !errors.Is(err, context.Canceled) {
		t.Fatalf("write: %v", err)
	}
	if err := indexWith(untilReceived(t, n[1]), nodes[1]); err == nil {
		t.Fatal("a stalled /status answered")
	}
	n[1].handle("/status", raft("Leader", "16", "n2"))
	for range 2 {
		if err := indexWith(within(t, 10*time.Second), nodes[1]); err != nil {
			t.Fatal(err)
		}
	}
	expectFacts(t, read,
		fact{"write_unanswered", true, "n2", 0},
		fact{"first_node_answers", false, "n2", 0},
		fact{"first_node_answers", true, "n2", 0})
}

func TestAWriteNotSentOrAnsweredWithHeadersIsNotUnanswered(t *testing.T) {
	read := factSink(t)
	n := fakeNodes(t)
	useWriteFacts(t)
	n[0].handle("/db/execute", n[0].stall(false))
	if err := writeWith(untilReceived(t, n[0]), nodes[0]); err == nil {
		t.Fatal("a stalled write answered")
	}
	expired, cancel := context.WithCancel(context.Background())
	cancel()
	if err := writeWith(expired, nodes[0]); err == nil {
		t.Fatal("a cancelled write answered")
	}
	n[0].handle("/db/execute", n[0].stall(true))
	if err := writeWith(within(t, time.Second), nodes[0]); err == nil {
		t.Fatal("a write with a stalled body answered")
	}
	expectFacts(t, read,
		fact{"write_unanswered", true, "n1", 0},
		fact{"write_unanswered", false, "?", 0},
		fact{"first_node_answers", true, "n1", 0},
		fact{"write_unanswered", false, "n1", 0})
}

func TestAWriteSentToAConnectionThatClosesIsUnanswered(t *testing.T) {
	read := factSink(t)
	n := fakeNodes(t)
	useWriteFacts(t)
	n[0].handle("/db/execute", hangUp(t))
	if err := writeWith(within(t, 10*time.Second), nodes[0]); err == nil || errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("write: %v", err)
	}
	expectFacts(t, read, fact{"write_unanswered", true, "n1", 0})
}

func TestInvokeChecksWritesAndCAS(t *testing.T) {
	read := factSink(t)
	n := fakeNodes(t)
	useWriteFacts(t)
	n[0].handle("/db/execute", func(w http.ResponseWriter, _ *http.Request) {
		_, _ = io.WriteString(w, `{"results":[{"rows_affected":1}]}`)
	})
	c := newClient(nodes[0])
	defer c.Close()
	for _, op := range []family.Op{
		{F: family.RegisterWrite, K: "k1", V: "v1"},
		{F: family.RegisterCAS, K: "k1", V: []string{"v1", "v2"}},
	} {
		if _, err := c.Invoke(within(t, 10*time.Second), op); err != nil {
			t.Fatalf("%v: %v", op.F, err)
		}
	}
	expectFacts(t, read, fact{"write_unanswered", false, "n1", 0}, fact{"write_unanswered", false, "n1", 0})
}

func TestWriteFactsAreSilentWithoutTheVariable(t *testing.T) {
	read := factSink(t)
	n := fakeNodes(t)
	n[0].handle("/db/execute", n[0].stall(false))
	n[0].handle("/status", raft("Leader", "15", "n1"))
	if err := writeWith(untilReceived(t, n[0]), nodes[0]); err == nil {
		t.Fatal("a stalled write answered")
	}
	if err := indexWith(within(t, 10*time.Second), nodes[0]); err != nil {
		t.Fatal(err)
	}
	expectFacts(t, read)
}

var snapshotFactIDs = []string{"snapshot_on_deposed_leader", "stale_tail", "trailing_within_tail", "client_on_node", "snapshot_at_commit_with_tail", "leader_snapshot_below", "follower_compacted_ahead"}

func snapshotConditions(t *testing.T, read func() []fact) []bool {
	t.Helper()
	got := read()
	if len(got) != len(snapshotFactIDs) {
		t.Fatalf("facts %v", got)
	}
	conds := make([]bool, len(got))
	for i, f := range got {
		if f.id != snapshotFactIDs[i] {
			t.Fatalf("fact %d is %s, want %s", i, f.id, snapshotFactIDs[i])
		}
		conds[i] = f.cond
	}
	return conds
}

func runSnapshotFacts(via int, trailing uint64, set bool, before, after []raftView) *snapshotFacts {
	return runSnapshotFactsWith("", via, trailing, set, before, after)
}

func runSnapshotFactsWith(connections string, via int, trailing uint64, set bool, before, after []raftView) *snapshotFacts {
	bases := []string{"b1", "b2", "b3"}
	at := func(views []raftView) func(context.Context, string) raftView {
		return func(_ context.Context, base string) raftView { return views[slices.Index(bases, base)] }
	}
	f := &snapshotFacts{sample: at(before), connections: connections}
	f.start(context.Background(), bases, via, trailing, set)
	f.sample = at(after)
	f.end(context.Background())
	return f
}

func view(state, leader string, term, commit, last, snapshot uint64) raftView {
	return raftView{answered: true, state: state, leader: leader, term: term, commit: commit, last: last, snapshot: snapshot}
}

func expectConditions(t *testing.T, got []bool, want ...bool) {
	t.Helper()
	if !slices.Equal(got, want) {
		t.Fatalf("conditions %v, want %v for %v", got, want, snapshotFactIDs)
	}
}

func TestSnapshotFactsSeeADeposedLeaderSnapshotAtCommitWithATail(t *testing.T) {
	read := factSink(t)
	leader := view("Leader", "n2", 3, 600, 600, 333)
	follower := view("Follower", "n2", 3, 600, 600, 405)
	runSnapshotFacts(0, 1, true,
		[]raftView{view("Candidate", "", 2, 482, 484, 0), leader, follower},
		[]raftView{view("Candidate", "", 2, 482, 484, 482), leader, follower})
	expectConditions(t, snapshotConditions(t, read), true, true, true, false, true, true, false)
}

func TestSnapshotFactsSeeAFollowerCompactAheadOfTheLeader(t *testing.T) {
	read := factSink(t)
	leader := view("Leader", "n1", 6, 1889, 1889, 1777)
	runSnapshotFacts(2, 1, true,
		[]raftView{leader, view("Follower", "n1", 6, 1814, 1815, 0), view("Follower", "n1", 6, 1814, 1814, 0)},
		[]raftView{leader, view("Follower", "n1", 6, 1815, 1815, 0), view("Follower", "n1", 6, 1814, 1814, 1814)})
	expectConditions(t, snapshotConditions(t, read), false, false, false, false, false, true, true)
}

func TestEitherALowerTermOrNoLeaderMakesADeposedLeader(t *testing.T) {
	leader := view("Leader", "n2", 3, 600, 600, 333)
	for _, c := range []struct {
		x       raftView
		deposed bool
	}{
		{view("Follower", "n2", 2, 600, 600, 0), true},
		{view("Follower", "", 3, 600, 600, 0), true},
		{view("Follower", "n2", 3, 600, 600, 0), false},
	} {
		read := factSink(t)
		runSnapshotFacts(0, 1, true, []raftView{c.x, leader, {}}, []raftView{c.x, leader, {}})
		expectConditions(t, snapshotConditions(t, read), c.deposed, false, false, false, false, false, false)
	}
}

func TestAFollowerAlreadyAheadHasNotCompactedAhead(t *testing.T) {
	read := factSink(t)
	leader := view("Leader", "n1", 6, 1889, 1889, 1777)
	x := view("Follower", "n1", 6, 1814, 1814, 1800)
	runSnapshotFacts(2, 1, true, []raftView{leader, {}, x}, []raftView{leader, {}, x})
	expectConditions(t, snapshotConditions(t, read), false, false, false, false, false, true, false)
}

func TestALeaderSnapshotEqualToTheNodesIsNotBelow(t *testing.T) {
	read := factSink(t)
	leader := view("Leader", "n1", 6, 1889, 1889, 1777)
	runSnapshotFacts(2, 1, true,
		[]raftView{leader, {}, view("Follower", "n1", 6, 1777, 1777, 0)},
		[]raftView{leader, {}, view("Follower", "n1", 6, 1777, 1777, 1777)})
	expectConditions(t, snapshotConditions(t, read), false, false, false, false, false, false, false)
}

func TestSnapshotFactsNeedTrailingLogsWithinTheTail(t *testing.T) {
	for _, c := range []struct {
		trailing uint64
		set      bool
		within   bool
	}{{2, true, true}, {3, true, false}, {0, false, false}} {
		read := factSink(t)
		x := view("Leader", "n1", 2, 480, 482, 0)
		runSnapshotFacts(0, c.trailing, c.set, []raftView{x, {}, {}}, []raftView{x, {}, {}})
		expectConditions(t, snapshotConditions(t, read), false, true, c.within, false, false, false, false)
	}
}

func TestSnapshotFactsAreFalseWhenTheNodeDoesNotAnswer(t *testing.T) {
	read := factSink(t)
	leader := view("Leader", "n2", 3, 600, 600, 333)
	runSnapshotFacts(0, 1, true, []raftView{{}, leader, {}}, []raftView{{}, leader, {}})
	expectConditions(t, snapshotConditions(t, read), false, false, false, false, false, false, false)
}

func TestClientOnNodeReadsEachProcessLatestConnection(t *testing.T) {
	path := filepath.Join(t.TempDir(), "connections")
	log := &connectionLog{path: path}
	for _, c := range []struct{ process, at int }{{0, 2}, {1, 1}, {2, 0}, {0, 1}} {
		if err := log.record(c.process, c.at); err != nil {
			t.Fatal(err)
		}
	}
	x := view("Leader", "n3", 2, 480, 482, 0)
	for via, want := range []bool{true, true, false} {
		read := factSink(t)
		views := []raftView{{}, {}, {}}
		views[via] = x
		f := runSnapshotFactsWith(path, via, 1, true, views, views)
		if f.err != nil {
			t.Fatal(f.err)
		}
		if got := snapshotConditions(t, read)[3]; got != want {
			t.Fatalf("client_on_node for %s is %v", nodeNames[via], got)
		}
	}
}

func TestAnUnreadableConnectionRecordFailsTheHook(t *testing.T) {
	read := factSink(t)
	path := filepath.Join(t.TempDir(), "connections")
	if err := os.WriteFile(path, []byte("0 n9\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	x := view("Leader", "n1", 2, 480, 482, 0)
	f := runSnapshotFactsWith(path, 0, 1, true, []raftView{x, {}, {}}, []raftView{x, {}, {}})
	if f.err == nil {
		t.Fatal("no error for a bad connection record")
	}
	if snapshotConditions(t, read)[3] {
		t.Fatal("client_on_node held without a record")
	}
}

func TestAConnectionRecordThatFailsPartwayCountsNoClient(t *testing.T) {
	read := factSink(t)
	path := filepath.Join(t.TempDir(), "connections")
	if err := os.WriteFile(path, []byte("0 n1\n"+strings.Repeat("x", 70000)+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	x := view("Leader", "n1", 2, 480, 482, 0)
	f := runSnapshotFactsWith(path, 0, 1, true, []raftView{x, {}, {}}, []raftView{x, {}, {}})
	if f.err == nil {
		t.Fatal("no error for an unreadable connection record")
	}
	if snapshotConditions(t, read)[3] {
		t.Fatal("client_on_node held from part of a record")
	}
}

func TestClientOnNodeIsFalseWhenTheNodeDoesNotAnswer(t *testing.T) {
	read := factSink(t)
	path := filepath.Join(t.TempDir(), "connections")
	if err := os.WriteFile(path, []byte("0 n1\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	leader := view("Leader", "n2", 3, 600, 600, 333)
	f := runSnapshotFactsWith(path, 0, 1, true, []raftView{{}, leader, {}}, []raftView{{}, leader, {}})
	if f.err != nil {
		t.Fatal(f.err)
	}
	expectConditions(t, snapshotConditions(t, read), false, false, false, false, false, false, false)
}

func TestTheDriveRecordsEachConnectionOnlyWithTheVariable(t *testing.T) {
	dir := t.TempDir()
	saved := connections
	t.Cleanup(func() { connections = saved })
	t.Setenv(connectionsEnv, "")
	if newConnectionLog() != nil {
		t.Fatal("a connection record without the variable")
	}
	t.Setenv(connectionsEnv, "1")
	if got := newConnectionLog(); got == nil || got.path != clientConnections {
		t.Fatalf("connection record %+v", got)
	}
	connections = &connectionLog{path: filepath.Join(dir, "connections")}
	want := map[int]string{}
	for process := range 3 {
		c, err := adapter{}.Connect(context.Background(), process)
		if err != nil {
			t.Fatal(err)
		}
		want[process] = nodeName(slices.Index(nodes, c.(*client).base))
		c.Close()
	}
	current, err := readConnections(connections.path)
	if err != nil || !maps.Equal(current, want) {
		t.Fatalf("connections %v, want %v, error %v", current, want, err)
	}
}

func TestAFailedConnectionRecordStillConnects(t *testing.T) {
	saved := connections
	t.Cleanup(func() { connections = saved })
	connections = &connectionLog{path: filepath.Join(t.TempDir(), "missing", "connections")}
	c, err := adapter{}.Connect(context.Background(), 0)
	if err != nil {
		t.Fatal(err)
	}
	c.Close()
}

func drawTrace(t *testing.T, facts bool) string {
	t.Helper()
	bases := make([]string, len(nodeNames))
	for i := range nodeNames {
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if r.URL.Path == "/status" {
				fmt.Fprint(w, `{"store":{"raft":{"state":"Follower","term":"2","commit_index":"480","last_log_index":"482","last_snapshot_index":"0"},"leader":{"node_id":""}}}`)
			}
		}))
		t.Cleanup(server.Close)
		bases[i] = server.URL
	}
	saved := connections
	t.Cleanup(func() { connections = saved })
	connections = nil
	var hook *snapshotFacts
	if facts {
		path := filepath.Join(t.TempDir(), "connections")
		connections = &connectionLog{path: path}
		hook = &snapshotFacts{sample: raftViewOf, connections: path}
	}
	var trace []string
	for process := range 3 {
		c, err := adapter{}.Connect(context.Background(), process)
		if err != nil {
			t.Fatal(err)
		}
		trace = append(trace, c.(*client).base)
		c.Close()
	}
	var out bytes.Buffer
	if err := recordSnapshot(&out, bases, hook); err != nil {
		t.Fatal(err)
	}
	fields := strings.Fields(out.String())
	return strings.Join(append(trace, fields[1], fields[2], strconv.FormatUint(sdk.Random(), 10)), " ")
}

func TestTheFactsAddNoDraw(t *testing.T) {
	if mode := os.Getenv("RQLITE_DRAW_MODE"); mode != "" {
		fmt.Println("trace", drawTrace(t, mode == "facts"))
		return
	}
	traces := map[string]string{}
	for _, mode := range []string{"plain", "facts"} {
		cmd := exec.Command(os.Args[0], "-test.run=^TestTheFactsAddNoDraw$")
		cmd.Env = append(os.Environ(), "RQLITE_DRAW_MODE="+mode, "HARNESS_SEED=7", "ANTITHESIS_OUTPUT_DIR="+t.TempDir())
		out, err := cmd.Output()
		if err != nil {
			t.Fatalf("%s: %v\n%s", mode, err, out)
		}
		for _, line := range strings.Split(string(out), "\n") {
			if strings.HasPrefix(line, "trace ") {
				traces[mode] = line
			}
		}
	}
	if traces["plain"] == "" || traces["plain"] != traces["facts"] {
		t.Fatalf("plain %q, facts %q", traces["plain"], traces["facts"])
	}
}

func TestSnapshotFactsCompareWithTheLeaderInTheHighestTerm(t *testing.T) {
	read := factSink(t)
	x := view("Follower", "n3", 4, 700, 700, 0)
	stale := view("Leader", "", 3, 600, 602, 100)
	current := view("Leader", "n3", 4, 700, 700, 650)
	runSnapshotFacts(0, 1, true,
		[]raftView{x, stale, current},
		[]raftView{view("Follower", "n3", 4, 700, 700, 640), stale, current})
	expectConditions(t, snapshotConditions(t, read), false, false, false, false, false, false, false)
}

func TestRaftViewDecodesNumbersAndStrings(t *testing.T) {
	server, _ := leaderServer(t,
		answer(http.StatusOK, `{"store":{"raft":{"state":"Follower","term":"6","commit_index":1814,"last_log_index":"1815","last_snapshot_index":"1777"},"leader":{"node_id":"n1"}}}`),
		answer(http.StatusOK, `{"store":{"open":false}}`),
		answer(http.StatusServiceUnavailable, ""),
		answer(http.StatusOK, `{"store":{"raft":{"state":"Follower","term":"6","commit_index":"null","last_log_index":"1815","last_snapshot_index":"1777"},"leader":{"node_id":"n1"}}}`))
	want := view("Follower", "n1", 6, 1814, 1815, 1777)
	if got := raftViewOf(context.Background(), server.URL); got != want {
		t.Fatalf("view %+v, want %+v", got, want)
	}
	for range 3 {
		if got := raftViewOf(context.Background(), server.URL); got.answered {
			t.Fatalf("view %+v from a node without Raft statistics", got)
		}
	}
}

func TestTheSnapshotHookReadsEveryNodeBeforeAndAfterItsRequest(t *testing.T) {
	read := factSink(t)
	var mu sync.Mutex
	var order []string
	var bases []string
	snapped := -1
	for i := range nodeNames {
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			mu.Lock()
			defer mu.Unlock()
			order = append(order, fmt.Sprintf("%s %s %s", nodeNames[i], r.Method, r.URL.Path))
			switch r.URL.Path {
			case "/snapshot":
				snapped = i
			case "/status":
				snapshot := 0
				if snapped == i {
					snapshot = 480
				}
				fmt.Fprintf(w, `{"store":{"raft":{"state":"Follower","term":"2","commit_index":"480","last_log_index":"482","last_snapshot_index":"%d"},"leader":{"node_id":""}}}`, snapshot)
			}
		}))
		t.Cleanup(server.Close)
		bases = append(bases, server.URL)
	}
	var out bytes.Buffer
	if err := recordSnapshot(&out, bases, &snapshotFacts{sample: raftViewOf}); err != nil {
		t.Fatal(err)
	}
	if snapped < 0 || !strings.Contains(out.String(), "node="+nodeNames[snapped]+" ") || !strings.HasSuffix(out.String(), " status 200 \"\"\n") {
		t.Fatalf("line %q for node %d", out.String(), snapped)
	}
	post := slices.Index(order, nodeNames[snapped]+" POST /snapshot")
	for _, name := range nodeNames {
		if at := slices.Index(order, name+" GET /status"); at < 0 || at > post {
			t.Fatalf("order %v", order)
		}
		if last := slices.Index(order[post+1:], name+" GET /status"); last < 0 {
			t.Fatalf("order %v", order)
		}
	}
	if len(order) != 7 {
		t.Fatalf("order %v", order)
	}
	within := slices.Contains([]string{"trailing_logs=1", "trailing_logs=2"}, strings.Fields(out.String())[2])
	expectConditions(t, snapshotConditions(t, read), false, true, within, false, true, false, false)
}
