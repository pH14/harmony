// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"errors"
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
