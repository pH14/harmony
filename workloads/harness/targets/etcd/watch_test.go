// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"

	"harmony.local/harness/kit/runner"
)

func watchMember(t *testing.T, kvs string, frames ...string) client {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/v3/kv/range":
			fmt.Fprintf(w, `{"header":{"revision":"30"},"kvs":%s}`, kvs)
		case "/v3/watch":
			var body struct {
				Create struct {
					Start uint64 `json:"start_revision"`
				} `json:"create_request"`
			}
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				t.Errorf("watch body: %v", err)
			}
			flusher := w.(http.Flusher)
			for _, frame := range frames {
				fmt.Fprintln(w, frame)
				flusher.Flush()
			}
		default:
			t.Errorf("unexpected path %s", r.URL.Path)
		}
	}))
	t.Cleanup(server.Close)
	return client{members: []string{server.URL}, names: []string{"m1"}, http: newHTTP(false)}
}

func eventFrame(revision uint64, keys ...string) string {
	events := ""
	for index, key := range keys {
		if index > 0 {
			events += ","
		}
		events += fmt.Sprintf(`{"kv":{"key":%q,"value":%q,"mod_revision":"%d"}}`, enc(key), enc("v"), revision)
	}
	return fmt.Sprintf(`{"result":{"events":[%s]}}`, events)
}

func bounded(t *testing.T) context.Context {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	t.Cleanup(cancel)
	return ctx
}

func TestAnEventWithoutARevisionIsAnError(t *testing.T) {
	c := watchMember(t, `[]`, `{"result":{"events":[{"kv":{"key":"azA="}}]}}`)
	events, err := c.stream(bounded(t), c.members[0], 10, time.Now().Add(time.Second), 0)
	if err == nil || len(events) != 0 {
		t.Fatalf("events %+v, error %v", events, err)
	}
}

func TestAHistoryReadKeepsEveryEventOfItsLastRevision(t *testing.T) {
	c := watchMember(t, `[{"key":"azA=","mod_revision":"20"},{"key":"azE=","mod_revision":"20"}]`,
		eventFrame(19, "k0"), eventFrame(20, "k0", "k1"))
	events, err := c.history(bounded(t), c.members[0])
	if err != nil {
		t.Fatal(err)
	}
	if len(events) != 3 || events[1].Key != "k0" || events[2].Key != "k1" || events[2].Revision != 20 {
		t.Fatalf("events %+v", events)
	}
}

func TestAWatchKeepsEventsFromABrokenStreamWithTheirStart(t *testing.T) {
	c := watchMember(t, `[]`, eventFrame(12, "k0"))
	got, err := c.watch(bounded(t), c.members[0], 12)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) == 0 || got[0].Revision != 12 || got[0].Start != 12 {
		t.Fatalf("events %+v", got)
	}
	for _, event := range got[1:] {
		if event.Start <= got[0].Revision {
			t.Fatalf("a resumed segment started at %d after revision %d", event.Start, got[0].Revision)
		}
	}
}

func TestAWatchThatDeliversNothingFails(t *testing.T) {
	c := watchMember(t, `[]`, `{"error":{"message":"etcdserver: mvcc: required revision has been compacted"}}`)
	got, err := c.watch(bounded(t), c.members[0], 12)
	if !runner.IsDefinite(err) {
		t.Fatalf("events %+v, error %v", got, err)
	}
}

func TestAFrameWithOneBadEventDeliversNoneOfIt(t *testing.T) {
	c := watchMember(t, `[]`, eventFrame(11, "k0"), `{"result":{"events":[{"kv":{"key":"azA=","value":"dg==","mod_revision":"12"}},{"kv":{"key":"azE="}}]}}`)
	events, err := c.stream(bounded(t), c.members[0], 10, time.Now().Add(time.Second), 0)
	if err == nil || len(events) != 1 || events[0].Revision != 11 {
		t.Fatalf("events %+v, error %v", events, err)
	}
}

// held answers puts, and holds every watch open until its request ends, after
// sending frame on the first watch it serves.
type held struct {
	mu     sync.Mutex
	starts []uint64
	frame  string
}

func (h *held) serve(t *testing.T) string {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/v3/kv/put":
			fmt.Fprint(w, `{"header":{"revision":"40"}}`)
		case "/v3/watch":
			var body struct {
				Create struct {
					Start uint64 `json:"start_revision"`
				} `json:"create_request"`
			}
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				t.Errorf("watch body: %v", err)
			}
			h.mu.Lock()
			h.starts = append(h.starts, body.Create.Start)
			first := len(h.starts) == 1
			h.mu.Unlock()
			if first && h.frame != "" {
				fmt.Fprintln(w, h.frame)
				return
			}
			w.(http.Flusher).Flush()
			<-r.Context().Done()
		default:
			t.Errorf("unexpected path %s", r.URL.Path)
		}
	}))
	t.Cleanup(server.Close)
	return server.URL
}

func TestAWatchFailsOverFromABrokenSegmentAtTheNextRevision(t *testing.T) {
	h := &held{frame: eventFrame(12, "k0")}
	first, second := h.serve(t), h.serve(t)
	c := client{members: []string{first, second}, names: []string{"m1", "m2"}, http: newHTTP(false)}
	got, err := c.segmented(bounded(t), first, 12, 2)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || got[0].Revision != 12 || got[0].Start != 12 {
		t.Fatalf("events %+v", got)
	}
	h.mu.Lock()
	defer h.mu.Unlock()
	if len(h.starts) != 2 || h.starts[0] != 12 || h.starts[1] != 13 {
		t.Fatalf("watch starts %v", h.starts)
	}
}

func TestAWatchThatStopsAtItsDeadlineLeavesAKeptConnectionUsable(t *testing.T) {
	recording(t)
	h := &held{}
	c := client{members: []string{h.serve(t)}, names: []string{"m1"}, http: newHTTP(true)}
	ctx, cancel := context.WithTimeout(context.Background(), 400*time.Millisecond)
	defer cancel()
	got, err := c.segmented(ctx, c.members[0], 12, 1)
	if err != nil || len(got) != 0 {
		t.Fatalf("events %+v, error %v", got, err)
	}
	if _, err := c.post(bounded(t), c.members[0], "/v3/kv/put", map[string]any{"key": enc("k0"), "value": enc("v")}); err != nil {
		t.Fatal(err)
	}
}
