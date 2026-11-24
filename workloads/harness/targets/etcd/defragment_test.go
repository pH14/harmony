// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
)

func stalledDefragment(t *testing.T, puts *atomic.Int32) string {
	t.Helper()
	release := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = io.Copy(io.Discard, r.Body)
		switch r.URL.Path {
		case "/v3/maintenance/defragment":
			select {
			case <-r.Context().Done():
			case <-release:
			}
		case "/v3/kv/put":
			puts.Add(1)
			fmt.Fprint(w, `{"header":{"revision":"7"}}`)
		default:
			t.Errorf("unexpected path %s", r.URL.Path)
		}
	}))
	t.Cleanup(server.Close)
	t.Cleanup(func() { close(release) })
	return server.URL
}

func TestADefragmentGetsHalfTheRemainingTimeAndTheOperationStillRuns(t *testing.T) {
	recording(t)
	var puts atomic.Int32
	c := client{members: []string{stalledDefragment(t, &puts)}, names: []string{"m1"}, defragment: 1, http: newHTTP(false)}
	ctx, cancel := context.WithTimeout(context.Background(), 600*time.Millisecond)
	defer cancel()
	start := time.Now()
	if _, err := c.Invoke(ctx, family.Op{F: family.RegisterWrite, K: "k", V: "v"}); err != nil {
		t.Fatal(err)
	}
	if puts.Load() != 1 {
		t.Fatalf("puts %d", puts.Load())
	}
	if elapsed := time.Since(start); elapsed > 450*time.Millisecond {
		t.Fatalf("the defragment held the operation for %v", elapsed)
	}
}

func TestAnOperationWhoseDeadlinePassesDuringTheDefragmentFailsDefinitely(t *testing.T) {
	recording(t)
	var puts atomic.Int32
	c := client{members: []string{stalledDefragment(t, &puts)}, names: []string{"m1"}, defragment: 1, http: newHTTP(false)}
	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		time.Sleep(100 * time.Millisecond)
		cancel()
	}()
	_, err := c.Invoke(ctx, family.Op{F: family.RegisterWrite, K: "k", V: "v"})
	if !runner.IsDefinite(err) {
		t.Fatalf("error %v", err)
	}
	if puts.Load() != 0 {
		t.Fatalf("puts %d", puts.Load())
	}
}
