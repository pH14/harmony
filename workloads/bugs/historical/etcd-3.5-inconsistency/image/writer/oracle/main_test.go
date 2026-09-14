// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"
)

func TestOracleAcceptsOnlyTheFullHistoryCheck(t *testing.T) {
	if err := run([]string{"check", "journal"}); err == nil {
		t.Fatal("check accepted an endpoint-free invocation")
	}
	if err := run([]string{"window", "journal", "verified", "endpoint"}); err == nil {
		t.Fatal("oracle retained the removed window mode")
	}
	if err := run([]string{"sweep", "journal", "endpoint"}); err == nil {
		t.Fatal("oracle retained the removed sweep mode")
	}
}

func TestOnlyAcknowledgedWorkloadRecordsAreCompared(t *testing.T) {
	journal := strings.Join([]string{
		"museum/1/key-000000000007\tvalue-1-7\t17",
		"museum/1/key-000000000007\tvalue-1-7\t17",
		"museum/2/key-000000000009\tvalue-2-9\t23",
		"museum/0/key-000000000001\tvalue-0-1\t7",
		"museum/1/key-000000000007\tvalue-2-7\t17",
		"museum/1/key-000000000007\tvalue-1-8\t17",
		"museum/5/key-000000000001\tvalue-5-1\t7",
		"museum/1/key-000000000008\tnot-a-value\t17",
		"museum/1/key-000000000008\tvalue-1-8",
		"museum/1/key-000000000008\tvalue-1-8\t0",
		"museum/1/key-000000000008\tvalue-1-8\tno-revision",
		"museum/1/key-000000000008\tvalue-1-8\t17",
		"",
	}, "\n")

	entries := selectEntries(journal)
	want := []journalRecord{
		{key: "museum/1/key-000000000007", value: "value-1-7", revision: 17},
		{key: "museum/1/key-000000000008", value: "value-1-8", revision: 17},
		{key: "museum/2/key-000000000009", value: "value-2-9", revision: 23},
	}
	if !reflect.DeepEqual(entries, want) {
		t.Fatalf("selectEntries = %v, want %v", entries, want)
	}
}

func TestAKeyIsZeroPaddedToItsNumericOrder(t *testing.T) {
	if got := key("3", 42); got != "museum/3/key-000000000042" {
		t.Fatalf("key = %q", got)
	}
	if key("3", 9) >= key("3", 10) {
		t.Fatal("byte order disagrees with numeric order")
	}
}

func TestUnfencedMemberReadIsInconclusive(t *testing.T) {
	record := journalRecord{key: key("1", 7), value: "value-1-7", revision: 17}
	missing, stale := compareView([]journalRecord{record}, memberView{
		revision: 16,
		values:   map[string]string{},
	})
	if missing != 0 || !stale {
		t.Fatalf("compareView = (%d, %v), want (0, true)", missing, stale)
	}
}

func TestAKeyVerifiedBeforeALaterFaultIsCheckedAgain(t *testing.T) {
	record := journalRecord{key: key("1", 7), value: "value-1-7", revision: 17}
	expected := []journalRecord{record}
	first := memberView{
		revision: 17,
		values:   map[string]string{record.key: record.value},
	}
	missing, stale := compareView(expected, first)
	if missing != 0 || stale {
		t.Fatalf("initial compareView = (%d, %v), want (0, false)", missing, stale)
	}
	second := memberView{revision: 18, values: map[string]string{}}
	missing, stale = compareView(expected, second)
	if missing != 1 || stale {
		t.Fatalf("post-fault compareView = (%d, %v), want (1, false)", missing, stale)
	}
}

func TestAStableGenerationChecksOnlyNewAcknowledgements(t *testing.T) {
	first := "museum/1/key-000000000007\tvalue-1-7\t17\n"
	second := "museum/2/key-000000000009\tvalue-2-9\t23\n"
	records, state := selectCheckWindow([]byte(first), oracleState{}, false, 4, true)
	if len(records) != 1 || state.offset != int64(len(first)) || state.verified != 1 {
		t.Fatalf("first window = (%v, %+v)", records, state)
	}
	records, state = selectCheckWindow([]byte(first+second), state, true, 4, true)
	if len(records) != 1 || records[0].key != key("2", 9) || state.verified != 2 {
		t.Fatalf("incremental window = (%v, %+v)", records, state)
	}
}

func TestADisturbanceRechecksTheFullAcknowledgedHistory(t *testing.T) {
	first := "museum/1/key-000000000007\tvalue-1-7\t17\n"
	second := "museum/2/key-000000000009\tvalue-2-9\t23\n"
	_, state := selectCheckWindow([]byte(first), oracleState{}, false, 4, true)
	records, state := selectCheckWindow([]byte(first+second), state, true, 5, true)
	if len(records) != 2 || state.verified != 2 || state.generation != 5 {
		t.Fatalf("disturbed window = (%v, %+v)", records, state)
	}
}

func TestAnIncompleteJournalRecordRemainsForTheNextWindow(t *testing.T) {
	complete := "museum/1/key-000000000007\tvalue-1-7\t17\n"
	partial := "museum/2/key-000000000009\tvalue-2"
	_, state := selectCheckWindow([]byte(complete+partial), oracleState{}, false, 4, true)
	if state.offset != int64(len(complete)) {
		t.Fatalf("offset = %d, want %d", state.offset, len(complete))
	}
	records, state := selectCheckWindow([]byte(complete+partial+"-9\t23\n"), state, true, 4, true)
	if len(records) != 1 || records[0].key != key("2", 9) || state.verified != 2 {
		t.Fatalf("completed window = (%v, %+v)", records, state)
	}
}

func TestOracleStateRoundTrips(t *testing.T) {
	path := filepath.Join(t.TempDir(), "journal.verified")
	want := oracleState{offset: 73, verified: 11, generation: 5}
	if err := writeOracleState(path, want); err != nil {
		t.Fatal(err)
	}
	got, ok := readOracleState(path)
	if !ok || got != want {
		t.Fatalf("readOracleState = (%+v, %v), want (%+v, true)", got, ok, want)
	}
}

func TestAStaleMemberCanCatchUpAfterTheFormerPlateau(t *testing.T) {
	record := journalRecord{key: key("1", 7), value: "value-1-7", revision: 17}
	reads := 0
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	got := compareMember(ctx, []journalRecord{record}, func(context.Context) (memberView, error) {
		reads++
		if reads <= 16 {
			return memberView{revision: 16, values: map[string]string{}}, nil
		}
		return memberView{
			revision: 17,
			values:   map[string]string{record.key: record.value},
		}, nil
	}, func(context.Context, time.Duration) bool { return true })
	if got != verdictAgreed {
		t.Fatalf("compareMember = %v, want agreed", got)
	}
	if reads != 17 {
		t.Fatalf("compareMember made %d reads, want 17", reads)
	}
}

func TestAValueMissingAtItsAppliedFenceIsLoss(t *testing.T) {
	record := journalRecord{key: key("1", 7), value: "value-1-7", revision: 17}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	got := compareMember(ctx, []journalRecord{record}, func(context.Context) (memberView, error) {
		return memberView{revision: 18, values: map[string]string{}}, nil
	}, func(context.Context, time.Duration) bool { return true })
	if got != verdictLost {
		t.Fatalf("compareMember = %v, want lost", got)
	}
}

func TestAReadErrorIsInconclusive(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	got := compareMember(ctx, []journalRecord{{key: key("1", 1), value: "value-1-1", revision: 1}},
		func(context.Context) (memberView, error) {
			return memberView{}, context.DeadlineExceeded
		}, func(context.Context, time.Duration) bool { return true })
	if got != verdictInconclusive {
		t.Fatalf("compareMember = %v, want inconclusive", got)
	}
}
