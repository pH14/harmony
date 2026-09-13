// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"fmt"
	"os"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"

	clientv3 "go.etcd.io/etcd/client/v3"
)

const sequenceDigits = 12
const workloadWorkers = 4

const requestTimeout = 15 * time.Second
const settleInterval = 200 * time.Millisecond

const (
	reachablePoint = 11
	alwaysPoint    = 1
)

var (
	keyPattern   = regexp.MustCompile(`^museum/([1-9][0-9]*)/key-([0-9]+)$`)
	valuePattern = regexp.MustCompile(`^value-([1-9][0-9]*)-([0-9]+)$`)
)

type journalRecord struct {
	key      string
	value    string
	revision int64
}

type memberView struct {
	revision int64
	values   map[string]string
}

type verdict int

const (
	verdictInconclusive verdict = iota
	verdictAgreed
	verdictLost
)

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) < 3 {
		return fmt.Errorf("usage: etcd-oracle check <journal> <endpoint>...")
	}
	if args[0] != "check" {
		return fmt.Errorf("unknown mode %q", args[0])
	}
	return check(args[1], args[2:])
}

func check(journalPath string, endpoints []string) error {
	snapshot, err := os.ReadFile(journalPath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	expected := selectEntries(string(snapshot))
	if len(expected) == 0 {
		return nil
	}
	if compareAgainstMembers(expected, endpoints, readPrefix) == verdictAgreed {
		fmt.Printf("@verified %d\n", len(expected))
	}
	return nil
}

func compareAgainstMembers(
	expected []journalRecord,
	endpoints []string,
	read func(context.Context, *clientv3.Client) (memberView, error),
) verdict {
	if len(endpoints) == 0 {
		return verdictInconclusive
	}
	conclusive := true
	lost := false
	for _, endpoint := range endpoints {
		client, err := clientv3.New(clientv3.Config{
			Endpoints:   []string{endpoint},
			DialTimeout: requestTimeout,
		})
		if err != nil {
			conclusive = false
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), requestTimeout)
		current := compareMember(ctx, expected,
			func(ctx context.Context) (memberView, error) {
				return read(ctx, client)
			}, waitForSettle)
		cancel()
		client.Close()
		switch current {
		case verdictInconclusive:
			conclusive = false
		case verdictLost:
			lost = true
		}
	}
	if !conclusive {
		return verdictInconclusive
	}
	fmt.Printf("@reachable %d\n", reachablePoint)
	if lost {
		fmt.Printf("@always %d 0\n", alwaysPoint)
		return verdictLost
	}
	fmt.Printf("@always %d 1\n", alwaysPoint)
	return verdictAgreed
}

func compareMember(
	ctx context.Context,
	expected []journalRecord,
	read func(context.Context) (memberView, error),
	wait func(context.Context, time.Duration) bool,
) verdict {
	for {
		if err := ctx.Err(); err != nil {
			return verdictInconclusive
		}
		view, err := read(ctx)
		if err != nil {
			return verdictInconclusive
		}
		missing, stale := compareView(expected, view)
		if !stale {
			if missing == 0 {
				return verdictAgreed
			}
			return verdictLost
		}
		if !wait(ctx, settleInterval) {
			return verdictInconclusive
		}
	}
}

func compareView(expected []journalRecord, view memberView) (int, bool) {
	missing := 0
	stale := false
	for _, record := range expected {
		if view.revision < record.revision {
			stale = true
			continue
		}
		value, ok := view.values[record.key]
		if !ok || value != record.value {
			missing++
		}
	}
	return missing, stale
}

func waitForSettle(ctx context.Context, interval time.Duration) bool {
	timer := time.NewTimer(interval)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}

func readPrefix(ctx context.Context, client *clientv3.Client) (memberView, error) {
	response, err := client.Get(ctx, "museum/",
		clientv3.WithPrefix(), clientv3.WithSerializable())
	if err != nil {
		return memberView{}, err
	}
	if response == nil || response.Header == nil {
		return memberView{}, fmt.Errorf("member read did not include a revision")
	}
	values := make(map[string]string, len(response.Kvs))
	for _, pair := range response.Kvs {
		values[string(pair.Key)] = string(pair.Value)
	}
	return memberView{revision: response.Header.Revision, values: values}, nil
}

func selectEntries(text string) []journalRecord {
	unique := make(map[journalRecord]struct{})
	for _, line := range strings.Split(text, "\n") {
		fields := strings.Split(line, "\t")
		if len(fields) != 3 {
			continue
		}
		revision, err := strconv.ParseInt(fields[2], 10, 64)
		if err != nil || revision < 1 || !workloadRecord(fields[0], fields[1], revision) {
			continue
		}
		unique[journalRecord{key: fields[0], value: fields[1], revision: revision}] = struct{}{}
	}
	records := make([]journalRecord, 0, len(unique))
	for record := range unique {
		records = append(records, record)
	}
	sort.Slice(records, func(i, j int) bool {
		if records[i].key != records[j].key {
			return records[i].key < records[j].key
		}
		if records[i].revision != records[j].revision {
			return records[i].revision < records[j].revision
		}
		return records[i].value < records[j].value
	})
	return records
}

func workloadRecord(key, value string, revision int64) bool {
	if revision < 1 {
		return false
	}
	keyFields := keyPattern.FindStringSubmatch(key)
	valueFields := valuePattern.FindStringSubmatch(value)
	if keyFields == nil || valueFields == nil {
		return false
	}
	keyWorker, err := strconv.Atoi(keyFields[1])
	if err != nil || keyWorker < 1 || keyWorker > workloadWorkers {
		return false
	}
	valueWorker, err := strconv.Atoi(valueFields[1])
	if err != nil || valueWorker != keyWorker {
		return false
	}
	keySequence, err := strconv.Atoi(keyFields[2])
	if err != nil || keySequence < 1 {
		return false
	}
	valueSequence, err := strconv.Atoi(valueFields[2])
	return err == nil && valueSequence == keySequence
}

func key(worker string, sequence int) string {
	return fmt.Sprintf("museum/%s/key-%0*d", worker, sequenceDigits, sequence)
}
