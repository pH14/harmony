// SPDX-License-Identifier: AGPL-3.0-or-later

// etcd-oracle compares what the workload acknowledged against every member's
// local view and prints the case's verdict.
//
// The check runs on the guest's single processor, competing with the three
// members it is reading. A shell implementation spawns a process for each
// step -- the watermark, the window, the record selection, a client per member
// range, the sort and the diff -- and those spawns cost more than the reads
// and the comparison do. The cost lands exactly when a member has just
// restarted and the oracle is deciding whether it is behind or has lost data,
// which is the moment the workload most needs the processor. One process doing
// the whole check leaves it there.
package main

import (
	"bytes"
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

// Sequence numbers are zero padded so a key's byte order matches its numeric
// order, which is what makes one range per worker exact.
const sequenceDigits = 12

// The check runs only when every member is up, so this deadline does not have
// to fail fast on a member that is gone. It has to be long enough for a member
// that is up to answer while it shares one emulated processor with the other
// two and with the workload, and short enough that three members still answer
// inside one check heartbeat.
const requestTimeout = 15 * time.Second

// A restarted member reports itself healthy while it is still applying the
// entries it missed, so a single read of a lagging member misses acknowledged
// keys it will hold moments later. The two cases are told apart by whether the
// member converges: applying shrinks the missing set, and a key the member will
// never hold keeps it at the same size. Each member is therefore read until its
// missing set empties or stops shrinking.
const (
	settlePlateauReads = 15
	settleInterval     = 200 * time.Millisecond
)

// The assertion points this case declares. They name positions in the source
// of truth for the bug, not positions in this file.
const (
	reachablePoint = 11
	alwaysPoint    = 1
)

// Workers append concurrently and a write can be torn, so only complete,
// workload-shaped records are compared.
var (
	keyPattern   = regexp.MustCompile(`^museum/[1-9][0-9]*/key-[0-9]+$`)
	valuePattern = regexp.MustCompile(`^value-[1-9][0-9]*-[0-9]+$`)
)

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(args []string) error {
	if len(args) < 3 {
		return fmt.Errorf(
			"usage: etcd-oracle window <journal> <verified> <endpoint>... | " +
				"etcd-oracle sweep <journal> <endpoint>...")
	}
	switch args[0] {
	case "window":
		if len(args) < 4 {
			return fmt.Errorf("window needs a journal, a watermark file and an endpoint")
		}
		return checkWindow(args[1], args[2], args[3:])
	case "sweep":
		return checkSweep(args[1], args[2:])
	default:
		return fmt.Errorf("unknown mode %q", args[0])
	}
}

// checkWindow verifies what has been acknowledged since the last passing
// check. The watermark is a byte offset, so the check reads only the bytes
// appended since then and never the whole journal. A check that re-read the
// history would cost more on every run and would eventually consume the
// processor the workload needs. The sweep catches what a window stepped over.
func checkWindow(journalPath, verifiedPath string, endpoints []string) error {
	journal, err := os.Open(journalPath)
	if err != nil {
		// A workload that has not written yet is not a failure.
		return nil
	}
	defer journal.Close()

	info, err := journal.Stat()
	if err != nil {
		return err
	}
	start, confirmed, err := readWatermark(verifiedPath)
	if err != nil {
		return err
	}
	if info.Size() <= start {
		return nil
	}

	appended := make([]byte, info.Size()-start)
	if _, err := journal.ReadAt(appended, start); err != nil {
		return err
	}
	window, consumed := windowOf(appended)
	if consumed == 0 {
		return nil
	}

	expected := selectEntries(window)
	if len(expected) == 0 {
		return nil
	}
	bounds := windowBounds(expected)

	verdict := compareAgainstMembers(expected, endpoints,
		func(ctx context.Context, client *clientv3.Client) (map[string]struct{}, error) {
			return readSpans(ctx, client, bounds)
		})
	if verdict != verdictAgreed {
		return nil
	}
	// Advance the watermark only when every member was read and agreed. The
	// record count travels with it so the host can see how much of the load the
	// oracle has actually confirmed, which no assertion carries.
	confirmed += int64(len(expected))
	line := fmt.Sprintf("%d\n%d\n", start+consumed, confirmed)
	if err := os.WriteFile(verifiedPath, []byte(line), 0o644); err != nil {
		return err
	}
	fmt.Printf("@verified %d\n", confirmed)
	return nil
}

// checkSweep compares the entire journal against every member. It runs once at
// the end of a measurement, so a loss no window covered is still reported.
func checkSweep(journalPath string, endpoints []string) error {
	snapshot, err := os.ReadFile(journalPath)
	if err != nil {
		return nil
	}
	expected := selectEntries(string(snapshot))
	if len(expected) == 0 {
		return nil
	}
	compareAgainstMembers(expected, endpoints, readPrefix)
	return nil
}

type verdict int

const (
	verdictInconclusive verdict = iota
	verdictAgreed
	verdictLost
)

// compareAgainstMembers reads every member until it agrees with `expected` or
// stops converging, then emits the case's verdict.
//
// A failed read or an unreachable member leaves the oracle inconclusive and
// silent: a member that is still recovering is not evidence of data loss.
func compareAgainstMembers(
	expected []string,
	endpoints []string,
	read func(context.Context, *clientv3.Client) (map[string]struct{}, error),
) verdict {
	// A check with no member to read decides nothing.
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
		remaining := -1
		plateau := 0
		for {
			ctx, cancel := context.WithTimeout(context.Background(), requestTimeout)
			held, err := read(ctx, client)
			cancel()
			if err != nil {
				conclusive = false
				break
			}
			missing := countMissing(expected, held)
			if missing == 0 {
				break
			}
			if remaining < 0 || missing < remaining {
				remaining = missing
				plateau = 0
			} else {
				plateau++
				if plateau >= settlePlateauReads {
					lost = true
					break
				}
			}
			time.Sleep(settleInterval)
		}
		client.Close()
	}
	if !conclusive {
		return verdictInconclusive
	}
	fmt.Printf("@reachable %d\n", reachablePoint)
	if !lost {
		fmt.Printf("@always %d 1\n", alwaysPoint)
		return verdictAgreed
	}
	fmt.Printf("@always %d 0\n", alwaysPoint)
	return verdictLost
}

func countMissing(expected []string, held map[string]struct{}) int {
	missing := 0
	for _, entry := range expected {
		if _, ok := held[entry]; !ok {
			missing++
		}
	}
	return missing
}

// readSpans reads one range per writer worker. Keys are zero padded, so one
// range request isolates exactly the span that worker contributed to the
// window, and one connection answers them all.
func readSpans(
	ctx context.Context,
	client *clientv3.Client,
	bounds []span,
) (map[string]struct{}, error) {
	held := make(map[string]struct{})
	for _, bound := range bounds {
		from := key(bound.worker, bound.low)
		// A range end is exclusive.
		to := key(bound.worker, bound.high+1)
		response, err := client.Get(ctx, from,
			clientv3.WithRange(to), clientv3.WithSerializable())
		if err != nil {
			return nil, err
		}
		collect(held, response)
	}
	return held, nil
}

// readPrefix reads a member's whole `museum/` prefix.
func readPrefix(ctx context.Context, client *clientv3.Client) (map[string]struct{}, error) {
	response, err := client.Get(ctx, "museum/",
		clientv3.WithPrefix(), clientv3.WithSerializable())
	if err != nil {
		return nil, err
	}
	held := make(map[string]struct{})
	collect(held, response)
	return held, nil
}

func collect(held map[string]struct{}, response *clientv3.GetResponse) {
	for _, pair := range response.Kvs {
		held[string(pair.Key)+"\t"+string(pair.Value)] = struct{}{}
	}
}

// readWatermark returns the journal offset already verified and the record
// count confirmed up to it. A missing or unreadable watermark starts the
// oracle from the beginning of the journal rather than failing the check.
func readWatermark(path string) (int64, int64, error) {
	text, err := os.ReadFile(path)
	if err != nil {
		return 0, 0, nil
	}
	lines := strings.Split(string(text), "\n")
	return countAt(lines, 0), countAt(lines, 1), nil
}

func countAt(lines []string, index int) int64 {
	if index >= len(lines) {
		return 0
	}
	value, err := strconv.ParseInt(lines[index], 10, 64)
	if err != nil || value < 0 {
		return 0
	}
	return value
}

// windowOf returns the bytes of `appended` that end on a record boundary, and
// how many of them that is. Workers append concurrently, so the journal's last
// line can be a partial write; ending the window on a newline leaves the torn
// record for the next window and skips no record.
func windowOf(appended []byte) (string, int64) {
	end := bytes.LastIndexByte(appended, '\n') + 1
	return string(appended[:end]), int64(end)
}

// selectEntries keeps the complete, workload-shaped records in `text` and
// returns them sorted and deduplicated, as `key<tab>value` lines.
func selectEntries(text string) []string {
	unique := make(map[string]struct{})
	for _, line := range strings.Split(text, "\n") {
		fields := strings.Split(line, "\t")
		if len(fields) != 2 {
			continue
		}
		if !keyPattern.MatchString(fields[0]) || !valuePattern.MatchString(fields[1]) {
			continue
		}
		unique[line] = struct{}{}
	}
	entries := make([]string, 0, len(unique))
	for entry := range unique {
		entries = append(entries, entry)
	}
	sort.Strings(entries)
	return entries
}

type span struct {
	worker    string
	low, high int
}

// windowBounds reduces the window to the sequence span each worker contributed
// to it, in worker order.
func windowBounds(expected []string) []span {
	low := make(map[string]int)
	high := make(map[string]int)
	for _, entry := range expected {
		name, _, _ := strings.Cut(entry, "\t")
		path := strings.Split(name, "/")
		if len(path) != 3 {
			continue
		}
		worker := path[1]
		sequence, err := strconv.Atoi(strings.TrimPrefix(path[2], "key-"))
		if err != nil {
			continue
		}
		if existing, ok := low[worker]; !ok || sequence < existing {
			low[worker] = sequence
		}
		if existing, ok := high[worker]; !ok || sequence > existing {
			high[worker] = sequence
		}
	}
	workers := make([]string, 0, len(low))
	for worker := range low {
		workers = append(workers, worker)
	}
	sort.Strings(workers)
	bounds := make([]span, 0, len(workers))
	for _, worker := range workers {
		bounds = append(bounds, span{worker: worker, low: low[worker], high: high[worker]})
	}
	return bounds
}

func key(worker string, sequence int) string {
	return fmt.Sprintf("museum/%s/key-%0*d", worker, sequenceDigits, sequence)
}
