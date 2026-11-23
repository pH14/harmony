// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
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

func member(t *testing.T, revision *int, status *int) string {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(*status)
		fmt.Fprintf(w, `{"header":{"revision":"%d"}}`, *revision)
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

func TestALinearizableRangeBelowAnAcknowledgedPutFails(t *testing.T) {
	got := recording(t)
	ahead, behind, ok := 155, 3, http.StatusOK
	c := client{http: newHTTP()}
	ctx := context.Background()
	fresh, stale := member(t, &ahead, &ok), member(t, &behind, &ok)

	if _, err := c.post(ctx, fresh, "/v3/kv/put", map[string]any{"key": enc("k"), "value": enc("v")}); err != nil {
		t.Fatal(err)
	}
	if _, err := c.post(ctx, stale, "/v3/kv/range", map[string]any{"key": enc("k")}); err != nil {
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

func TestSerializableRangesAndFailedAnswersAreNotChecked(t *testing.T) {
	got := recording(t)
	ahead, behind, ok, unavailable := 155, 3, http.StatusOK, http.StatusServiceUnavailable
	c := client{http: newHTTP()}
	ctx := context.Background()
	fresh, stale, down := member(t, &ahead, &ok), member(t, &behind, &ok), member(t, &behind, &unavailable)

	if _, err := c.post(ctx, fresh, "/v3/kv/put", map[string]any{"key": enc("k"), "value": enc("v")}); err != nil {
		t.Fatal(err)
	}
	if _, err := c.post(ctx, stale, "/v3/kv/range", map[string]any{"key": enc("k"), "serializable": true}); err != nil {
		t.Fatal(err)
	}
	if _, err := c.post(ctx, down, "/v3/kv/range", map[string]any{"key": enc("k")}); err == nil {
		t.Fatal("a 503 answer was accepted")
	}

	if len(*got) != 1 || len(failed(*got)) != 0 {
		t.Fatalf("assertions %v", *got)
	}
}

func TestARevisionAcknowledgedAfterTheRequestWasSentIsNotAFloor(t *testing.T) {
	got := recording(t)
	ahead, behind, ok := 155, 3, http.StatusOK
	c := client{http: newHTTP()}
	ctx := context.Background()
	arrived, release := make(chan struct{}), make(chan struct{})
	slow := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		arrived <- struct{}{}
		<-release
		fmt.Fprintf(w, `{"header":{"revision":"%d"}}`, behind)
	}))
	t.Cleanup(slow.Close)
	fresh, stale := member(t, &ahead, &ok), member(t, &behind, &ok)

	done := make(chan error)
	go func() {
		_, err := c.post(ctx, slow.URL, "/v3/kv/range", map[string]any{"key": enc("k")})
		done <- err
	}()
	<-arrived
	if _, err := c.post(ctx, fresh, "/v3/kv/put", map[string]any{"key": enc("k"), "value": enc("v")}); err != nil {
		t.Fatal(err)
	}
	close(release)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if len(*got) != 2 || len(failed(*got)) != 0 {
		t.Fatalf("assertions %v", *got)
	}
	if high, _ := acknowledged.floor(); high != 155 {
		t.Fatalf("floor %d after a lower late answer", high)
	}

	if _, err := c.post(ctx, stale, "/v3/kv/range", map[string]any{"key": enc("k")}); err != nil {
		t.Fatal(err)
	}
	if bad := failed(*got); len(*got) != 3 || len(bad) != 1 || bad[0].details["acknowledged"] != uint64(155) {
		t.Fatalf("assertions %v", *got)
	}
}

func TestAnAnswerAtTheFloorAndAFailedCompareAreChecked(t *testing.T) {
	got := recording(t)
	revision, ok := 155, http.StatusOK
	c := client{http: newHTTP()}
	ctx := context.Background()
	m := member(t, &revision, &ok)

	if _, err := c.post(ctx, m, "/v3/kv/put", map[string]any{"key": enc("k"), "value": enc("v")}); err != nil {
		t.Fatal(err)
	}
	if _, err := c.post(ctx, m, "/v3/kv/range", map[string]any{"key": enc("k")}); err != nil {
		t.Fatal(err)
	}
	r, err := c.post(ctx, m, "/v3/kv/txn", map[string]any{"compare": []any{}, "success": []any{}})
	if err != nil {
		t.Fatal(err)
	}
	if r.Succeeded {
		t.Fatal("a reply without succeeded decoded as succeeded")
	}
	if len(*got) != 3 || len(failed(*got)) != 0 {
		t.Fatalf("assertions %v", *got)
	}
	revision = 154
	if _, err := c.post(ctx, m, "/v3/kv/txn", map[string]any{"compare": []any{}, "success": []any{}}); err != nil {
		t.Fatal(err)
	}
	if len(*got) != 4 || len(failed(*got)) != 1 {
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
