// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"errors"
	"testing"
)

type delivery struct {
	subject string
	pending uint64
	value   string
}

func fetcher(t *testing.T, deliveries []delivery) func() (string, uint64, string, error) {
	return func() (string, uint64, string, error) {
		if len(deliveries) == 0 {
			t.Fatal("fetched past the last delivery")
		}
		d := deliveries[0]
		deliveries = deliveries[1:]
		return d.subject, d.pending, d.value, nil
	}
}

func TestCollectLastStopsWhenNothingIsPending(t *testing.T) {
	msgs, err := collectLast(2, fetcher(t, []delivery{
		{keySubject + "k0", 4, "a"},
		{keySubject + "k1", 3, "b"},
		{keySubject + "k0", 2, "c"},
		{keySubject + "fence-n1", 1, "f"},
		{keySubject + "k2", 0, "d"},
	}))
	if err != nil {
		t.Fatal(err)
	}
	want := map[string]string{"k0": "c", "k1": "b", "k2": "d", "fence-n1": "f"}
	if len(msgs) != len(want) {
		t.Fatalf("msgs %v", msgs)
	}
	for key, value := range want {
		if msgs[key] != value {
			t.Fatalf("msgs %v", msgs)
		}
	}
}

func TestCollectLastReadsPastTheInitialCount(t *testing.T) {
	msgs, err := collectLast(1, fetcher(t, []delivery{
		{keySubject + "fence-n2", 1, "f"},
		{keySubject + "k0", 0, "a"},
	}))
	if err != nil || msgs["k0"] != "a" {
		t.Fatalf("msgs %v err %v", msgs, err)
	}
}

func TestCollectLastEmptyConsumerFetchesNothing(t *testing.T) {
	msgs, err := collectLast(0, fetcher(t, nil))
	if err != nil || len(msgs) != 0 {
		t.Fatalf("msgs %v err %v", msgs, err)
	}
}

func TestCollectLastIgnoresOtherSubjects(t *testing.T) {
	msgs, err := collectLast(1, fetcher(t, []delivery{{"other.k0", 0, "x"}}))
	if err != nil || len(msgs) != 0 {
		t.Fatalf("msgs %v err %v", msgs, err)
	}
}

func TestCollectLastReturnsFetchErrors(t *testing.T) {
	failure := errors.New("down")
	_, err := collectLast(1, func() (string, uint64, string, error) { return "", 0, "", failure })
	if !errors.Is(err, failure) {
		t.Fatalf("err %v", err)
	}
}
