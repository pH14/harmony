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
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"harmony.local/harness/kit/family"
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

func TestAppliedIndexReadsOnlyTheIndex(t *testing.T) {
	server, _ := leaderServer(t, answer(http.StatusOK, `{"store":{"db_applied_index":12,"raft":{"state":7,"term":null}}}`))
	c := newClient(server.URL)
	defer c.Close()
	if applied, err := c.appliedIndex(context.Background()); err != nil || applied != 12 {
		t.Fatalf("applied %d, error %v", applied, err)
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
	if newStepdownFacts() != nil || newWriteFacts() != nil {
		t.Fatal("facts without the variable")
	}
	t.Setenv(factsEnv, "1")
	facts, w := newStepdownFacts(), newWriteFacts()
	if facts == nil || facts.path != transferTimeouts || facts.sample == nil || w == nil || w.pending == nil {
		t.Fatalf("stepdown facts %+v, write facts %+v", facts, w)
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
	_, err := c.appliedIndex(ctx)
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
