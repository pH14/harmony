// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"slices"
	"sync"
	"testing"
	"time"
)

func TestStatusAnswersNameTheLastAnswerAcrossAttempts(t *testing.T) {
	answers := &statusAnswers{last: map[string]statusAnswer{}}
	start := time.Unix(100, 0)
	answers.observe("n3", 1121, start)
	err := answers.err("n3", errors.New("fence write to n2: context deadline exceeded"), start.Add(2500*time.Millisecond))
	want := "n3 last answered /status at index 1121, 2.50s before this error; last attempt: fence write to n2: context deadline exceeded"
	if err.Error() != want {
		t.Fatalf("error = %q, want %q", err.Error(), want)
	}
}

func TestStatusAnswersKeepTheLatestAnswer(t *testing.T) {
	answers := &statusAnswers{last: map[string]statusAnswer{}}
	start := time.Unix(100, 0)
	answers.observe("n1", 5337, start)
	answers.observe("n1", 5522, start.Add(5*time.Second))
	err := answers.err("n1", context.DeadlineExceeded, start.Add(6*time.Second))
	want := "n1 last answered /status at index 5522, 1.00s before this error; last attempt: context deadline exceeded"
	if err.Error() != want {
		t.Fatalf("error = %q, want %q", err.Error(), want)
	}
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatal("the error does not wrap the attempt's error")
	}
}

func TestStatusAnswersWithoutAnAnswerSaySo(t *testing.T) {
	answers := &statusAnswers{last: map[string]statusAnswer{}}
	answers.observe("n2", 40, time.Unix(100, 0))
	err := answers.err("n1", errors.New("waiting for fence 5535: connection refused"), time.Unix(105, 0))
	want := "n1 has no /status answer on record; last attempt: waiting for fence 5535: connection refused"
	if err.Error() != want {
		t.Fatalf("error = %q, want %q", err.Error(), want)
	}
}

type stepdownRequest struct {
	path  string
	query string
	body  string
}

func leaderServer(t *testing.T, answers ...func(http.ResponseWriter)) (*httptest.Server, func() []stepdownRequest) {
	t.Helper()
	var mu sync.Mutex
	var requests []stepdownRequest
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Error(err)
		}
		mu.Lock()
		requests = append(requests, stepdownRequest{r.URL.Path, r.URL.RawQuery, string(body)})
		n := len(requests)
		mu.Unlock()
		if n > len(answers) {
			t.Errorf("request %d past the last answer", n)
			w.WriteHeader(http.StatusTeapot)
			return
		}
		answers[n-1](w)
	}))
	t.Cleanup(server.Close)
	return server, func() []stepdownRequest {
		mu.Lock()
		defer mu.Unlock()
		return slices.Clone(requests)
	}
}

func answer(status int, body string) func(http.ResponseWriter) {
	return func(w http.ResponseWriter) {
		w.WriteHeader(status)
		_, _ = io.WriteString(w, body)
	}
}

var (
	timedOut   = answer(http.StatusInternalServerError, `{"error": "leadership transfer timeout"}`)
	inProgress = answer(http.StatusInternalServerError, `{"error": "leadership transfer in progress"}`)
	accepted   = answer(http.StatusOK, "")
)

func runRetrying(server *httptest.Server, id string, retries int) []stepdownAnswer {
	var answers []stepdownAnswer
	c := newClient(server.URL)
	defer c.Close()
	c.stepdownRetrying(context.Background(), id, retries, func() time.Duration { return 0 }, func(a stepdownAnswer) {
		answers = append(answers, a)
	})
	return answers
}

func TestStepdownRetriesATimedOutTransfer(t *testing.T) {
	server, requests := leaderServer(t, timedOut, accepted)
	answers := runRetrying(server, "n2", 2)
	if len(answers) != 2 || !answers[0].timedOut() || answers[1].status != http.StatusOK {
		t.Fatalf("answers %v", answers)
	}
	for _, r := range requests() {
		if r.path != "/leader" || r.query != "wait&redirect" || r.body != `{"id":"n2"}` {
			t.Fatalf("request %+v", r)
		}
	}
}

func TestStepdownStopsAtItsRetryCount(t *testing.T) {
	server, requests := leaderServer(t, timedOut, timedOut)
	if answers := runRetrying(server, "n1", 1); len(answers) != 2 {
		t.Fatalf("answers %v", answers)
	}
	if len(requests()) != 2 {
		t.Fatalf("requests %v", requests())
	}
}

func TestStepdownDoesNotRetryOtherRefusals(t *testing.T) {
	server, _ := leaderServer(t, inProgress)
	answers := runRetrying(server, "n3", 2)
	if len(answers) != 1 || answers[0].timedOut() {
		t.Fatalf("answers %v", answers)
	}
}

func TestStepdownWithoutTargetSendsNoBodyAndFollowsOneRedirect(t *testing.T) {
	leader, leaderRequests := leaderServer(t, accepted)
	follower, _ := leaderServer(t, func(w http.ResponseWriter) {
		w.Header().Set("Location", leader.URL+"/leader?wait&redirect")
		w.WriteHeader(http.StatusMovedPermanently)
	})
	answers := runRetrying(follower, "", 0)
	if len(answers) != 1 || answers[0].status != http.StatusOK {
		t.Fatalf("answers %v", answers)
	}
	if got := leaderRequests(); len(got) != 1 || got[0].body != "" {
		t.Fatalf("leader requests %v", got)
	}
}
