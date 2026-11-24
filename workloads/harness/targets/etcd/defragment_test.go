// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"io"
	"net/http"
	"strings"
	"testing"
	"testing/synctest"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
)

type sent struct {
	path     string
	at       time.Time
	deadline time.Time
}

// stalled answers puts at once and holds a defragment for hold, or until its
// request is cancelled when hold is zero.
type stalled struct {
	hold     time.Duration
	requests []sent
}

func (s *stalled) RoundTrip(r *http.Request) (*http.Response, error) {
	deadline, _ := r.Context().Deadline()
	s.requests = append(s.requests, sent{r.URL.Path, time.Now(), deadline})
	if r.URL.Path == "/v3/maintenance/defragment" {
		if s.hold == 0 {
			<-r.Context().Done()
			return nil, r.Context().Err()
		}
		time.Sleep(s.hold)
	}
	return &http.Response{
		StatusCode: http.StatusOK,
		Body:       io.NopCloser(strings.NewReader(`{"header":{"revision":"7"}}`)),
		Request:    r,
	}, nil
}

func (s *stalled) paths() []string {
	var paths []string
	for _, r := range s.requests {
		paths = append(paths, r.path)
	}
	return paths
}

func defragmenting(s *stalled) client {
	return client{members: []string{"http://m1"}, names: []string{"m1"}, defragment: 1, http: &http.Client{Transport: s}}
}

func write(c client, ctx context.Context) error {
	_, err := c.Invoke(ctx, family.Op{F: family.RegisterWrite, K: "k", V: "v"})
	return err
}

func TestADefragmentGetsHalfTheRemainingTimeAndThePutFollowsIt(t *testing.T) {
	recording(t)
	synctest.Test(t, func(t *testing.T) {
		s := &stalled{}
		start := time.Now()
		ctx, cancel := context.WithTimeout(context.Background(), 600*time.Millisecond)
		defer cancel()
		if err := write(defragmenting(s), ctx); err != nil {
			t.Fatal(err)
		}
		if got := strings.Join(s.paths(), " "); got != "/v3/maintenance/defragment /v3/kv/put" {
			t.Fatalf("requests %s", got)
		}
		if got := s.requests[0].deadline.Sub(start); got != 300*time.Millisecond {
			t.Fatalf("defragment deadline %v after the start", got)
		}
		if got := s.requests[1].at.Sub(start); got != 300*time.Millisecond {
			t.Fatalf("put sent %v after the start", got)
		}
		if got := s.requests[1].deadline.Sub(start); got != 600*time.Millisecond {
			t.Fatalf("put deadline %v after the start", got)
		}
	})
}

func TestADefragmentWithoutADeadlineGetsTheLimit(t *testing.T) {
	recording(t)
	synctest.Test(t, func(t *testing.T) {
		s := &stalled{}
		start := time.Now()
		if err := write(defragmenting(s), context.Background()); err != nil {
			t.Fatal(err)
		}
		if got := s.requests[0].deadline.Sub(start); got != defragmentLimit {
			t.Fatalf("defragment deadline %v after the start", got)
		}
	})
}

func TestAnOperationWhoseDeadlinePassesDuringTheDefragmentFailsDefinitely(t *testing.T) {
	recording(t)
	synctest.Test(t, func(t *testing.T) {
		s := &stalled{hold: 700 * time.Millisecond}
		ctx, cancel := context.WithTimeout(context.Background(), 600*time.Millisecond)
		defer cancel()
		err := write(defragmenting(s), ctx)
		if !runner.IsDefinite(err) {
			t.Fatalf("error %v", err)
		}
		if got := strings.Join(s.paths(), " "); got != "/v3/maintenance/defragment" {
			t.Fatalf("requests %s", got)
		}
	})
}
