// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"encoding/json"
	"slices"
	"sort"

	"harmony.local/harness/kit/history"
)

const (
	RegisterWatchFloor    = "register: a watch never delivers an event below its start revision"
	RegisterWatchOrder    = "register: a watch delivers events in revision order, each once"
	RegisterWatchComplete = "register: a watch delivers every event from its start revision to its last delivered revision"
	RegisterWatchMatches  = "register: every delivered event matches the reference history"
	RegisterHistoryAgrees = "register: every member's history agrees"
)

const (
	RegisterWatch   = "watch"
	RegisterHistory = "history"
)

const watchReach = 100

type WatchQuery struct {
	From uint64 `json:"from"`
}

type Event struct {
	Revision uint64  `json:"revision"`
	Key      string  `json:"key"`
	Value    *string `json:"value"`
	Node     string  `json:"node,omitempty"`
	Start    uint64  `json:"start,omitempty"`
}

type Changefeed interface {
	Feed() (Op, bool)
}

func (r Register) Feed() (Op, bool) {
	return Op{F: RegisterHistory}, r.Watch > 0
}

func watchKinds() []string {
	return []string{RegisterWatchFloor, RegisterWatchOrder, RegisterWatchComplete, RegisterWatchMatches, RegisterHistoryAgrees}
}

func (g *registerGenerator) watchFrom(draw Draw) uint64 {
	switch draw("register.watch.from", 3) {
	case 0:
		return max(g.revision-min(g.revision, draw("register.watch.back", watchReach)), 1)
	case 1:
		return g.revision + 1
	}
	return g.revision + 2 + draw("register.watch.ahead", watchReach)
}

func (g *registerGenerator) observeRevision(result any) {
	switch value := result.(type) {
	case uint64:
		g.revision = max(g.revision, value)
	case []Event:
		for _, event := range value {
			g.revision = max(g.revision, event.Revision)
		}
	}
}

type watchOp struct {
	op     *history.Op
	from   uint64
	events []Event
}

type acknowledged struct {
	op       *history.Op
	key      string
	value    string
	revision uint64
	known    bool
}

func decodeEvents(op *history.Op) ([]Event, bool) {
	if op.Type != history.OK {
		return nil, false
	}
	var events []Event
	if json.Unmarshal(op.Output, &events) != nil {
		return nil, false
	}
	return events, true
}

func writeRevision(op *history.Op) (uint64, bool) {
	if op.Type != history.OK {
		return 0, false
	}
	var revision *uint64
	if json.Unmarshal(op.Output, &revision) != nil || revision == nil {
		return 0, false
	}
	return *revision, true
}

type feedHistory struct {
	op     *history.Op
	events map[uint64][]Event
	last   uint64
}

func newFeedHistory(op *history.Op, events []Event) feedHistory {
	byRevision := map[uint64][]Event{}
	var last uint64
	for _, event := range events {
		event.Node = ""
		event.Start = 0
		byRevision[event.Revision] = append(byRevision[event.Revision], event)
		last = max(last, event.Revision)
	}
	for _, at := range byRevision {
		sortEvents(at)
	}
	return feedHistory{op: op, events: byRevision, last: last}
}

func sortEvents(events []Event) {
	sort.Slice(events, func(i, j int) bool { return events[i].Key < events[j].Key })
}

func sameEvent(a, b Event) bool {
	if a.Key != b.Key || (a.Value == nil) != (b.Value == nil) {
		return false
	}
	return a.Value == nil || *a.Value == *b.Value
}

func sameEvents(a, b []Event) bool {
	if len(a) != len(b) {
		return false
	}
	for index := range a {
		if !sameEvent(a[index], b[index]) {
			return false
		}
	}
	return true
}

func (r Register) checkWatches(h *history.History, report *Report, found *collector) {
	var watches []watchOp
	var feeds []feedHistory
	writes := map[string]map[string]acknowledged{}
	for index := range h.Ops {
		op := &h.Ops[index]
		switch op.F {
		case RegisterWatch:
			var query WatchQuery
			events, ok := decodeEvents(op)
			if ok && json.Unmarshal(op.Input, &query) == nil {
				watches = append(watches, watchOp{op: op, from: query.From, events: events})
			}
		case RegisterHistory:
			if events, ok := decodeEvents(op); ok && op.Node != "" {
				feeds = append(feeds, newFeedHistory(op, events))
			}
		case RegisterWrite, RegisterCAS:
			decoded, ok := decodeRegister(op)
			if !ok {
				continue
			}
			revision, known := writeRevision(op)
			if writes[op.K] == nil {
				writes[op.K] = map[string]acknowledged{}
			}
			writes[op.K][decoded.input.value] = acknowledged{op: op, key: op.K, value: decoded.input.value, revision: revision, known: known}
		}
	}
	for _, watch := range watches {
		if len(watch.events) > 0 {
			report.Decided = true
			for _, kind := range []string{RegisterWatchFloor, RegisterWatchComplete, RegisterWatchMatches} {
				delete(report.Undecided, kind)
			}
		}
		if len(watch.events) > 1 {
			delete(report.Undecided, RegisterWatchOrder)
		}
	}
	sort.SliceStable(feeds, func(i, j int) bool { return feeds[i].last > feeds[j].last })
	nodes := map[string]bool{}
	for _, feed := range feeds {
		if feed.last > 0 {
			nodes[feed.op.Node] = true
		}
	}
	if len(nodes) >= 2 {
		report.Decided = true
		delete(report.Undecided, RegisterHistoryAgrees)
	}
	var reference *feedHistory
	if len(feeds) > 0 {
		reference = &feeds[0]
	}
	byRevision := map[uint64][]acknowledged{}
	for _, values := range writes {
		for _, write := range values {
			if write.known {
				byRevision[write.revision] = append(byRevision[write.revision], write)
			}
		}
	}
	for _, watch := range watches {
		checkWatch(watch, writes, byRevision, reference, found)
	}
	checkFeedsAgree(feeds, found)
	checkFeedsHoldWrites(feeds, byRevision, found)
}

func checkWatch(watch watchOp, writes map[string]map[string]acknowledged, byRevision map[uint64][]acknowledged, reference *feedHistory, found *collector) {
	evidence := func(details map[string]any) func() map[string]any {
		return func() map[string]any {
			details["watch"] = describe(watch.op)
			details["from"] = watch.from
			return details
		}
	}
	delivered := map[uint64][]Event{}
	var last uint64
	for at, event := range watch.events {
		if event.Revision < max(watch.from, event.Start) {
			found.add(RegisterWatchFloor, evidence(map[string]any{"event": event, "position": at}))
		}
		if at > 0 {
			previous := watch.events[at-1]
			repeated := slices.ContainsFunc(delivered[event.Revision], func(held Event) bool { return held.Key == event.Key })
			if event.Revision < previous.Revision || (event.Revision == previous.Revision && repeated) {
				found.add(RegisterWatchOrder, evidence(map[string]any{"event": event, "previous": previous, "position": at}))
			}
		}
		delivered[event.Revision] = append(delivered[event.Revision], event)
		last = max(last, event.Revision)
		if acknowledgedAt(event, byRevision) {
			continue
		}
		if reference != nil && event.Revision <= reference.last {
			want := reference.events[event.Revision]
			match := false
			for _, held := range want {
				match = match || sameEvent(held, event)
			}
			if !match {
				found.add(RegisterWatchMatches, evidence(map[string]any{"event": event, "reference": want, "reference_node": reference.op.Node}))
			}
			continue
		}
		if event.Value != nil && !matchesWrite(event, writes) {
			found.add(RegisterWatchMatches, evidence(map[string]any{"event": event}))
		}
	}
	if len(watch.events) == 0 {
		return
	}
	missing := func(revision uint64, want Event) {
		for _, held := range delivered[revision] {
			if sameEvent(held, want) {
				return
			}
		}
		found.add(RegisterWatchComplete, evidence(map[string]any{"missing": want, "last_delivered": last}))
	}
	revisions := map[uint64]bool{}
	if reference != nil {
		for revision := range reference.events {
			revisions[revision] = true
		}
	}
	for revision := range byRevision {
		revisions[revision] = true
	}
	ordered := make([]uint64, 0, len(revisions))
	for revision := range revisions {
		if revision >= watch.from && revision <= last {
			ordered = append(ordered, revision)
		}
	}
	slices.Sort(ordered)
	for _, revision := range ordered {
		if reference != nil && revision <= reference.last {
			for _, want := range reference.events[revision] {
				missing(revision, want)
			}
		}
		for _, write := range byRevision[revision] {
			value := write.value
			missing(revision, Event{Revision: revision, Key: write.key, Value: &value})
		}
	}
}

func acknowledgedAt(event Event, byRevision map[uint64][]acknowledged) bool {
	for _, write := range byRevision[event.Revision] {
		if write.key == event.Key && event.Value != nil && write.value == *event.Value {
			return true
		}
	}
	return false
}

// A member's history that covers an acknowledged write's revision without the
// write disagrees with the cluster's acknowledgment, whatever the other
// members hold.
func checkFeedsHoldWrites(feeds []feedHistory, byRevision map[uint64][]acknowledged, found *collector) {
	for _, feed := range feeds {
		for revision, writes := range byRevision {
			if revision > feed.last {
				continue
			}
			for _, write := range writes {
				value := write.value
				want := Event{Revision: revision, Key: write.key, Value: &value}
				if slices.ContainsFunc(feed.events[revision], func(held Event) bool { return sameEvent(held, want) }) {
					continue
				}
				found.addFor(RegisterHistoryAgrees, feed.op.Node, func() map[string]any {
					return map[string]any{
						"missing":        want,
						"write":          describe(write.op),
						"history_read":   describe(feed.op),
						"compared_up_to": feed.last,
					}
				})
			}
		}
	}
}

func matchesWrite(event Event, writes map[string]map[string]acknowledged) bool {
	write, ok := writes[event.Key][*event.Value]
	if !ok || write.op.Type == history.Fail {
		return false
	}
	return !write.known || write.revision == event.Revision
}

func checkFeedsAgree(feeds []feedHistory, found *collector) {
	for i := range feeds {
		for j := i + 1; j < len(feeds); j++ {
			compareFeeds(feeds[i], feeds[j], found)
		}
	}
}

func compareFeeds(a, b feedHistory, found *collector) {
	if a.op.Node == b.op.Node {
		return
	}
	limit := min(a.last, b.last)
	revisions := map[uint64]bool{}
	for revision := range a.events {
		revisions[revision] = revision <= limit
	}
	for revision := range b.events {
		revisions[revision] = revision <= limit
	}
	var differing []uint64
	for revision, within := range revisions {
		if within && !sameEvents(a.events[revision], b.events[revision]) {
			differing = append(differing, revision)
		}
	}
	if len(differing) == 0 {
		return
	}
	sort.Slice(differing, func(x, y int) bool { return differing[x] < differing[y] })
	first := differing[0]
	found.addFor(RegisterHistoryAgrees, b.op.Node, func() map[string]any {
		return map[string]any{
			"revision":       first,
			"revisions":      len(differing),
			"reference_node": a.op.Node,
			"reference":      a.events[first],
			"events":         b.events[first],
			"reference_read": describe(a.op),
			"history_read":   describe(b.op),
			"compared_up_to": limit,
		}
	})
}
