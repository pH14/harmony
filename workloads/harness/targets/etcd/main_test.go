// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"
)

type assertion struct {
	cond    bool
	details map[string]any
}

func recording(t *testing.T) *[]assertion {
	t.Helper()
	var mu sync.Mutex
	var got []assertion
	saved := acknowledged
	acknowledged = &revisions{assert: func(cond bool, property string, details map[string]any) {
		if property != revisionNeverFalls {
			t.Errorf("property %q", property)
		}
		mu.Lock()
		defer mu.Unlock()
		got = append(got, assertion{cond, details})
	}}
	t.Cleanup(func() { acknowledged = saved })
	return &got
}

func member(t *testing.T, revision, status int) string {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(status)
		fmt.Fprintf(w, `{"header":{"revision":"%d"}}`, revision)
	}))
	t.Cleanup(server.Close)
	return server.URL
}

func failed(got []assertion) []assertion {
	var out []assertion
	for _, a := range got {
		if !a.cond {
			out = append(out, a)
		}
	}
	return out
}

func high() uint64 {
	h, _ := acknowledged.floor()
	return h
}

func put() map[string]any    { return map[string]any{"key": enc("k"), "value": enc("v")} }
func ranged() map[string]any { return map[string]any{"key": enc("k")} }

func cas() map[string]any {
	return map[string]any{
		"compare": []any{map[string]any{"key": enc("k"), "result": "EQUAL", "target": "VALUE", "value": enc("old")}},
		"success": []any{map[string]any{"request_put": map[string]any{"key": enc("k"), "value": enc("new")}}},
	}
}

func TestALinearizableRangeBelowAnAcknowledgedPutFails(t *testing.T) {
	got := recording(t)
	c := client{http: newHTTP()}
	ctx := context.Background()
	fresh, stale := member(t, 155, http.StatusOK), member(t, 3, http.StatusOK)

	if _, err := c.post(ctx, fresh, "/v3/kv/put", put()); err != nil {
		t.Fatal(err)
	}
	if _, err := c.post(ctx, stale, "/v3/kv/range", ranged()); err != nil {
		t.Fatal(err)
	}

	bad := failed(*got)
	if len(*got) != 2 || len(bad) != 1 {
		t.Fatalf("assertions %v", *got)
	}
	d := bad[0].details
	if d["revision"] != uint64(3) || d["acknowledged"] != uint64(155) || d["member"] != stale || d["acknowledged_by"] != fresh {
		t.Fatalf("details %v", d)
	}
}

func TestSerializableRangesAndFailedAnswersNeitherAreCheckedNorRaiseTheFloor(t *testing.T) {
	got := recording(t)
	c := client{http: newHTTP()}
	ctx := context.Background()
	fresh := member(t, 155, http.StatusOK)
	ahead, down := member(t, 900, http.StatusOK), member(t, 900, http.StatusServiceUnavailable)

	if _, err := c.post(ctx, fresh, "/v3/kv/put", put()); err != nil {
		t.Fatal(err)
	}
	serializable := ranged()
	serializable["serializable"] = true
	if _, err := c.post(ctx, ahead, "/v3/kv/range", serializable); err != nil {
		t.Fatal(err)
	}
	if _, err := c.post(ctx, down, "/v3/kv/range", ranged()); err == nil {
		t.Fatal("a 503 answer was accepted")
	}

	if len(*got) != 1 || len(failed(*got)) != 0 {
		t.Fatalf("assertions %v", *got)
	}
	if high() != 155 {
		t.Fatalf("floor %d", high())
	}
}

func TestEachCheckedAnswerRaisesTheFloor(t *testing.T) {
	for _, first := range []struct {
		path string
		body map[string]any
	}{
		{"/v3/kv/put", put()},
		{"/v3/kv/range", ranged()},
		{"/v3/kv/txn", cas()},
	} {
		t.Run(first.path, func(t *testing.T) {
			got := recording(t)
			c := client{http: newHTTP()}
			ctx := context.Background()
			fresh, stale := member(t, 155, http.StatusOK), member(t, 154, http.StatusOK)

			r, err := c.post(ctx, fresh, first.path, first.body)
			if err != nil {
				t.Fatal(err)
			}
			if r.Succeeded {
				t.Fatal("a reply without succeeded decoded as succeeded")
			}
			if _, err := c.post(ctx, stale, "/v3/kv/range", ranged()); err != nil {
				t.Fatal(err)
			}
			if bad := failed(*got); len(*got) != 2 || len(bad) != 1 || bad[0].details["acknowledged"] != uint64(155) {
				t.Fatalf("assertions %v", *got)
			}
		})
	}
}

func TestAnAnswerAtTheFloorPasses(t *testing.T) {
	got := recording(t)
	c := client{http: newHTTP()}
	ctx := context.Background()
	m := member(t, 155, http.StatusOK)

	for _, step := range []struct {
		path string
		body map[string]any
	}{{"/v3/kv/put", put()}, {"/v3/kv/range", ranged()}, {"/v3/kv/txn", cas()}} {
		if _, err := c.post(ctx, m, step.path, step.body); err != nil {
			t.Fatal(err)
		}
	}
	if len(*got) != 3 || len(failed(*got)) != 0 {
		t.Fatalf("assertions %v", *got)
	}
}

func TestARevisionAcknowledgedAfterTheRequestWasSentIsNotAFloor(t *testing.T) {
	got := recording(t)
	c := client{http: newHTTP()}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	arrived, release := make(chan struct{}, 1), make(chan struct{})
	slow := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		arrived <- struct{}{}
		select {
		case <-release:
		case <-r.Context().Done():
			return
		}
		fmt.Fprint(w, `{"header":{"revision":"3"}}`)
	}))
	done := make(chan error, 1)
	t.Cleanup(func() {
		cancel()
		slow.Close()
	})
	fresh, stale := member(t, 155, http.StatusOK), member(t, 3, http.StatusOK)

	go func() {
		_, err := c.post(ctx, slow.URL, "/v3/kv/range", ranged())
		done <- err
	}()
	select {
	case <-arrived:
	case err := <-done:
		t.Fatalf("the slow range ended before its handler ran: %v", err)
	}
	if _, err := c.post(ctx, fresh, "/v3/kv/put", put()); err != nil {
		t.Fatal(err)
	}
	close(release)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if len(*got) != 2 || len(failed(*got)) != 0 {
		t.Fatalf("assertions %v", *got)
	}
	if high() != 155 {
		t.Fatalf("floor %d after a lower late answer", high())
	}

	if _, err := c.post(ctx, stale, "/v3/kv/range", ranged()); err != nil {
		t.Fatal(err)
	}
	if bad := failed(*got); len(*got) != 3 || len(bad) != 1 || bad[0].details["acknowledged"] != uint64(155) {
		t.Fatalf("assertions %v", *got)
	}
}

func TestOnlyMapBodiesWithoutSerializableAreChecked(t *testing.T) {
	for _, c := range []struct {
		path string
		body any
		want bool
	}{
		{"/v3/kv/range", map[string]any{"key": "k"}, true},
		{"/v3/kv/range", map[string]any{"key": "k", "serializable": true}, false},
		{"/v3/kv/range", map[string]any{"key": "k", "serializable": false}, false},
		{"/v3/kv/range", map[string]any{"key": "k", "serializable": nil}, false},
		{"/v3/kv/range", struct{ Serializable bool }{true}, false},
		{"/v3/kv/put", map[string]any{"key": "k"}, true},
		{"/v3/kv/txn", map[string]any{}, true},
		{"/v3/maintenance/status", map[string]any{}, false},
	} {
		if got := linearizable(c.path, c.body); got != c.want {
			t.Errorf("linearizable(%s, %v) = %v", c.path, c.body, got)
		}
	}
}
