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
	floor, from := acknowledged.floor()
	acknowledged.answered("m2", "/v3/kv/put", 0, "", 155)
	acknowledged.answered("m1", "/v3/kv/range", floor, from, 3)

	if len(failed(*got)) != 0 {
		t.Fatalf("assertions %v", *got)
	}
	acknowledged.answered("m1", "/v3/kv/range", 155, "m2", 3)
	if len(failed(*got)) != 1 {
		t.Fatalf("assertions %v", *got)
	}
}
