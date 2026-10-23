// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"encoding/json"
	"fmt"
	"math"
	"sort"
	"sync"

	"harmony.local/harness/kit/history"
)

const (
	LogLost         = "log: every acknowledged append is in every later read"
	LogReadLost     = "log: every value a read returned is in every later read"
	LogOrder        = "log: reads agree on one order of values"
	LogDuplicate    = "log: no read contains a value twice"
	LogPhantom      = "log: every value read was appended"
	LogFailedRead   = "log: no append that failed is in a read"
	LogPosition     = "log: acknowledged appends have distinct positions"
	LogNodeLost     = "log: every node's read includes every acknowledged append"
	LogNodeReadLost = "log: every node's read includes every value an earlier read returned"
	LogNodeOrder    = "log: each node's reads agree on one order of values"
	LogAppendAcked  = "log: an append was acknowledged"
)

const (
	LogAppend = "append"
	LogRead   = "read"
)

const (
	maxArenas  = 64
	maxMissing = 32
)

type Log struct{}

func (Log) Name() string { return "log" }

func (Log) Kinds() []string {
	return []string{LogLost, LogReadLost, LogOrder, LogDuplicate, LogPhantom, LogFailedRead, LogPosition, LogNodeLost, LogNodeReadLost, LogNodeOrder}
}

func (Log) Observations() []string { return []string{LogAppendAcked} }

func (Log) Read() Op { return Op{F: LogRead} }

func (Log) Generator(int) Generator { return logGenerator{} }

type logGenerator struct{}

func (logGenerator) Next(draw Draw, value func() string) Op {
	if draw(8) == 0 {
		return Op{F: LogRead}
	}
	return Op{F: LogAppend, V: value()}
}

func (logGenerator) Observe(Op, history.Type, any) {}

type ReadOutput struct {
	Base *history.Ref `json:"b,omitempty"`
	Keep int          `json:"keep"`
	Add  []string     `json:"add"`
	N    int          `json:"n"`
}

type logChain struct {
	base   *history.Ref
	values []string
}

type logRecorder struct {
	mu     sync.Mutex
	chains map[string]*logChain
}

func (Log) Recorder(prior *history.History) Recorder {
	recorder := &logRecorder{chains: map[string]*logChain{}}
	if prior == nil {
		return recorder
	}
	state := resolveLog(prior)
	latest := map[string]*logRead{}
	for index := range state.reads {
		read := &state.reads[index]
		if current := latest[read.op.Node]; current == nil || read.op.Complete > current.op.Complete {
			latest[read.op.Node] = read
		}
	}
	for node, read := range latest {
		recorder.chains[node] = &logChain{
			base:   &history.Ref{Journal: read.op.Journal, Index: read.op.Index},
			values: append([]string(nil), read.values()...),
		}
	}
	return recorder
}

func (r *logRecorder) Record(journal *history.Journal, p int, op Op, index int64, result any) error {
	if op.F != LogRead {
		return journal.Complete(p, history.OK, op.F, op.K, index, result)
	}
	values, ok := result.([]string)
	if !ok && result != nil {
		return fmt.Errorf("log read returned %T, want []string", result)
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	chain := r.chains[op.Node]
	if chain == nil {
		chain = &logChain{}
		if unpinned := r.chains[""]; unpinned != nil {
			*chain = *unpinned
		}
		r.chains[op.Node] = chain
	}
	output := ReadOutput{Base: chain.base, N: len(values)}
	if chain.base != nil {
		for output.Keep < len(values) && output.Keep < len(chain.values) && values[output.Keep] == chain.values[output.Keep] {
			output.Keep++
		}
	}
	output.Add = values[output.Keep:]
	if err := journal.Complete(p, history.OK, op.F, op.K, index, output); err != nil {
		return err
	}
	chain.base = &history.Ref{Journal: journal.Name(), Index: index}
	chain.values = append(chain.values[:0:0], values...)
	return nil
}

type arena struct {
	values  []string
	first   map[string]int
	repeats map[string]bool
	dup     int
	phantom int
	failed  int
	longest map[string]*logRead
}

func (a *arena) index(value string) (int, bool) {
	at, ok := a.first[value]
	return at, ok
}

type logRead struct {
	op    *history.Op
	arena *arena
	n     int
}

func (r *logRead) values() []string {
	if r.arena == nil {
		return nil
	}
	return r.arena.values[:r.n]
}

type logState struct {
	reads      []logRead
	arenas     []*arena
	unresolved int
}

type sequence struct {
	arena *arena
	n     int
}

func resolveLog(h *history.History) *logState {
	state := &logState{}
	memo := map[*history.Op]*sequence{}
	visiting := map[*history.Op]bool{}
	var resolve func(op *history.Op) *sequence
	resolve = func(op *history.Op) *sequence {
		if seq, ok := memo[op]; ok {
			return seq
		}
		if visiting[op] {
			return nil
		}
		visiting[op] = true
		seq := state.resolveOne(h, op, resolve)
		memo[op] = seq
		return seq
	}
	for index := range h.Ops {
		op := &h.Ops[index]
		if op.F != LogRead || op.Type != history.OK {
			continue
		}
		seq := resolve(op)
		if seq == nil {
			state.unresolved++
			continue
		}
		state.reads = append(state.reads, logRead{op: op, arena: seq.arena, n: seq.n})
	}
	for index := range state.reads {
		read := &state.reads[index]
		if read.arena == nil {
			continue
		}
		if current := read.arena.longest[read.op.Node]; current == nil || read.n > current.n {
			read.arena.longest[read.op.Node] = read
		}
	}
	return state
}

func (s *logState) resolveOne(h *history.History, op *history.Op, resolve func(*history.Op) *sequence) *sequence {
	var output ReadOutput
	if err := json.Unmarshal(op.Output, &output); err != nil {
		return nil
	}
	base := &sequence{}
	if output.Base != nil {
		parent, ok := h.Lookup(*output.Base)
		if !ok || parent.F != LogRead || parent.Type != history.OK {
			return nil
		}
		base = resolve(parent)
		if base == nil {
			return nil
		}
	} else if output.Keep != 0 {
		return nil
	}
	if output.Keep < 0 || output.Keep > base.n || output.Keep+len(output.Add) != output.N {
		return nil
	}
	candidate := base.arena
	if candidate == nil {
		for _, a := range s.arenas {
			if candidate == nil || len(a.values) > len(candidate.values) {
				candidate = a
			}
		}
	}
	return s.extend(candidate, output.Keep, output.Add)
}

func (s *logState) extend(a *arena, keep int, add []string) *sequence {
	if a != nil {
		matched := true
		for offset, value := range add {
			at := keep + offset
			if at == len(a.values) {
				a.values = append(a.values, add[offset:]...)
				break
			}
			if a.values[at] != value {
				matched = false
				break
			}
		}
		if matched {
			return &sequence{arena: a, n: keep + len(add)}
		}
	}
	if len(s.arenas) >= maxArenas {
		return nil
	}
	fresh := &arena{longest: map[string]*logRead{}}
	if a != nil {
		fresh.values = append(fresh.values, a.values[:keep]...)
	}
	fresh.values = append(fresh.values, add...)
	s.arenas = append(s.arenas, fresh)
	return &sequence{arena: fresh, n: len(fresh.values)}
}

func (a *arena) prepare(appended map[string]*history.Op) {
	a.first = make(map[string]int, len(a.values))
	a.repeats = map[string]bool{}
	a.dup = len(a.values)
	a.phantom = len(a.values)
	a.failed = len(a.values)
	for at, value := range a.values {
		if _, seen := a.first[value]; seen {
			a.repeats[value] = true
			if at < a.dup {
				a.dup = at
			}
		} else {
			a.first[value] = at
		}
		op, ok := appended[value]
		if !ok && at < a.phantom {
			a.phantom = at
		}
		if ok && op.Type == history.Fail && at < a.failed {
			a.failed = at
		}
	}
}

type ack struct {
	op       *history.Op
	value    string
	position *uint64
}

func (Log) Check(h *history.History) Report {
	state := resolveLog(h)
	appended := map[string]*history.Op{}
	var acks []ack
	report := Report{Facts: map[string]bool{}}
	for index := range h.Ops {
		op := &h.Ops[index]
		if op.F != LogAppend {
			continue
		}
		var value string
		if json.Unmarshal(op.Input, &value) != nil {
			continue
		}
		if _, seen := appended[value]; !seen {
			appended[value] = op
		}
		if op.Type != history.OK {
			continue
		}
		report.Facts[LogAppendAcked] = true
		entry := ack{op: op, value: value}
		var position uint64
		if len(op.Output) > 0 && string(op.Output) != "null" && json.Unmarshal(op.Output, &position) == nil {
			entry.position = &position
		}
		acks = append(acks, entry)
	}
	sort.SliceStable(acks, func(i, j int) bool { return acks[i].op.Complete < acks[j].op.Complete })
	for _, a := range state.arenas {
		a.prepare(appended)
	}
	report.Decided = len(state.reads) > 0
	report.Undecided = map[string]bool{LogLost: true, LogReadLost: true, LogOrder: true, LogNodeLost: true, LogNodeReadLost: true, LogNodeOrder: true}
	for index := range state.reads {
		if state.reads[index].op.Node == "" {
			delete(report.Undecided, LogLost)
			delete(report.Undecided, LogReadLost)
			delete(report.Undecided, LogOrder)
		} else {
			delete(report.Undecided, LogNodeLost)
			delete(report.Undecided, LogNodeReadLost)
			delete(report.Undecided, LogNodeOrder)
		}
	}
	var found collector
	checkPositions(acks, &found)
	checkReadContents(state, appended, &found)
	lost := checkLost(state, acks, &found)
	checkReadLost(state, lost, &found)
	checkOrder(state, &found)
	report.Violations = found.result()
	return report
}

func checkPositions(acks []ack, found *collector) {
	byPosition := map[uint64]*ack{}
	for index := range acks {
		entry := &acks[index]
		if entry.position == nil {
			continue
		}
		earlier, seen := byPosition[*entry.position]
		if !seen {
			byPosition[*entry.position] = entry
			continue
		}
		found.add(LogPosition, func() map[string]any {
			return map[string]any{
				"position": *entry.position,
				"first":    map[string]any{"value": earlier.value, "append": describe(earlier.op)},
				"second":   map[string]any{"value": entry.value, "append": describe(entry.op)},
			}
		})
	}
}

func checkReadContents(state *logState, appended map[string]*history.Op, found *collector) {
	for index := range state.reads {
		read := &state.reads[index]
		a := read.arena
		node := read.op.Node
		if a.dup < read.n {
			found.addFor(LogDuplicate, node, func() map[string]any {
				value := a.values[a.dup]
				return map[string]any{
					"value":     value,
					"positions": []int{a.first[value], a.dup},
					"read":      describeRead(read),
				}
			})
		}
		if a.phantom < read.n {
			found.addFor(LogPhantom, node, func() map[string]any {
				return map[string]any{
					"value":    a.values[a.phantom],
					"position": a.phantom,
					"read":     describeRead(read),
				}
			})
		}
		if a.failed < read.n {
			found.addFor(LogFailedRead, node, func() map[string]any {
				value := a.values[a.failed]
				return map[string]any{
					"value":    value,
					"position": a.failed,
					"append":   describe(appended[value]),
					"read":     describeRead(read),
				}
			})
		}
	}
}

func checkLost(state *logState, acks []ack, found *collector) map[*logRead]bool {
	lost := map[*logRead]bool{}
	reach := map[*arena][]int{}
	for _, a := range state.arenas {
		prefix := make([]int, len(acks))
		highest := -1
		for index, entry := range acks {
			at, ok := a.index(entry.value)
			if !ok {
				at = math.MaxInt
			}
			if at > highest {
				highest = at
			}
			prefix[index] = highest
		}
		reach[a] = prefix
	}
	for index := range state.reads {
		read := &state.reads[index]
		before := sort.Search(len(acks), func(i int) bool { return acks[i].op.Complete >= read.op.Invoke })
		if before == 0 || reach[read.arena][before-1] < read.n {
			continue
		}
		lost[read] = true
		kind := LogLost
		if read.op.Node != "" {
			kind = LogNodeLost
		}
		found.addFor(kind, read.op.Node, func() map[string]any {
			missing := 0
			var first *ack
			var values []string
			for at := 0; at < before; at++ {
				position, ok := read.arena.index(acks[at].value)
				if !ok || position >= read.n {
					missing++
					if first == nil {
						first = &acks[at]
					}
					if len(values) < maxMissing {
						values = append(values, acks[at].value)
					}
				}
			}
			details := map[string]any{
				"value":          first.value,
				"ack":            describe(first.op),
				"read":           describeRead(read),
				"missing":        missing,
				"missing_values": values,
			}
			if first.position != nil {
				details["position"] = *first.position
			}
			return details
		})
	}
	return lost
}

func checkReadLost(state *logState, lost map[*logRead]bool, found *collector) {
	var sources, reads []*logRead
	for index := range state.reads {
		read := &state.reads[index]
		if read.op.Node == "" && read.n > 0 && read.n <= read.arena.phantom && read.n <= read.arena.failed {
			sources = append(sources, read)
		}
		reads = append(reads, read)
	}
	if len(sources) == 0 {
		return
	}
	sort.SliceStable(sources, func(i, j int) bool { return sources[i].op.Complete < sources[j].op.Complete })
	sort.SliceStable(reads, func(i, j int) bool { return reads[i].op.Invoke < reads[j].op.Invoke })
	widest := map[*arena]*logRead{}
	var arenas []*arena
	reach := map[[2]*arena][]int{}
	next := 0
	for _, read := range reads {
		for next < len(sources) && sources[next].op.Complete < read.op.Invoke {
			source := sources[next]
			current := widest[source.arena]
			if current == nil {
				arenas = append(arenas, source.arena)
			}
			if current == nil || source.n > current.n {
				widest[source.arena] = source
			}
			next++
		}
		if lost[read] {
			continue
		}
		for _, a := range arenas {
			witness := widest[a]
			key := [2]*arena{a, read.arena}
			prefix, ok := reach[key]
			if !ok {
				prefix = arenaReach(a, read.arena)
				reach[key] = prefix
			}
			if prefix[witness.n-1] < read.n {
				continue
			}
			kind := LogReadLost
			if read.op.Node != "" {
				kind = LogNodeReadLost
			}
			found.addFor(kind, read.op.Node, func() map[string]any {
				missing := 0
				var first string
				firstAt := -1
				var values []string
				for at, value := range witness.values() {
					position, ok := read.arena.index(value)
					if ok && position < read.n {
						continue
					}
					missing++
					if firstAt < 0 {
						first, firstAt = value, at
					}
					if len(values) < maxMissing {
						values = append(values, value)
					}
				}
				return map[string]any{
					"value":          first,
					"position":       firstAt,
					"earlier_read":   describeRead(witness),
					"read":           describeRead(read),
					"missing":        missing,
					"missing_values": values,
				}
			})
			break
		}
	}
}

func arenaReach(from, to *arena) []int {
	prefix := make([]int, len(from.values))
	highest := -1
	for at, value := range from.values {
		position, ok := to.index(value)
		if !ok {
			position = math.MaxInt
		}
		if position > highest {
			highest = position
		}
		prefix[at] = highest
	}
	return prefix
}

func checkOrder(state *logState, found *collector) {
	byArena := map[*arena][]*logRead{}
	for index := range state.reads {
		read := &state.reads[index]
		byArena[read.arena] = append(byArena[read.arena], read)
	}
	for x, left := range state.arenas {
		for y, right := range state.arenas {
			if x < y {
				checkCommonOrder(left, right, found)
			}
			if x != y {
				checkInsertion(left, byArena[left], right, byArena[right], found)
			}
		}
	}
}

type orderReport struct {
	found    *collector
	reported map[string]bool
}

func (r *orderReport) add(first, second *logRead, details func() map[string]any) {
	evidence := func() map[string]any {
		values := details()
		values["first_read"] = describeRead(first)
		values["second_read"] = describeRead(second)
		return values
	}
	node := first.op.Node
	if second.op.Node != node || r.reported[node] {
		return
	}
	r.reported[node] = true
	if node == "" {
		r.found.add(LogOrder, evidence)
	} else {
		r.found.addFor(LogNodeOrder, node, evidence)
	}
}

func checkCommonOrder(left, right *arena, found *collector) {
	var nodes []string
	for node := range left.longest {
		if right.longest[node] != nil {
			nodes = append(nodes, node)
		}
	}
	sort.Strings(nodes)
	report := orderReport{found: found, reported: map[string]bool{}}
	for _, node := range nodes {
		first, second := left.longest[node], right.longest[node]
		highest, highestAt := -1, -1
		for at, value := range right.values[:second.n] {
			if right.repeats[value] || left.repeats[value] {
				continue
			}
			position, ok := left.index(value)
			if !ok || position >= first.n {
				continue
			}
			if position < highest {
				before, beforeAt := left.values[highest], highestAt
				report.add(first, second, func() map[string]any {
					return map[string]any{
						"values":                []string{before, value},
						"first_read_positions":  []int{highest, position},
						"second_read_positions": []int{beforeAt, at},
					}
				})
				break
			}
			highest, highestAt = position, at
		}
	}
}

type insertion struct{ inserted, observed int }

type witnesses struct {
	reads   []*logRead
	longest []*logRead
}

func checkInsertion(earlier *arena, earlierReads []*logRead, later *arena, laterReads []*logRead, found *collector) {
	if len(earlierReads) == 0 || len(laterReads) == 0 {
		return
	}
	byNode := map[string]*witnesses{}
	for _, read := range earlierReads {
		group := byNode[read.op.Node]
		if group == nil {
			group = &witnesses{}
			byNode[read.op.Node] = group
		}
		group.reads = append(group.reads, read)
	}
	for _, group := range byNode {
		sort.Slice(group.reads, func(i, j int) bool { return group.reads[i].op.Complete < group.reads[j].op.Complete })
		group.longest = make([]*logRead, len(group.reads))
		for index, read := range group.reads {
			group.longest[index] = read
			if index > 0 && group.longest[index-1].n >= read.n {
				group.longest[index] = group.longest[index-1]
			}
		}
	}
	thresholds := map[int]insertion{}
	report := orderReport{found: found, reported: map[string]bool{}}
	for _, read := range laterReads {
		group := byNode[read.op.Node]
		if group == nil || report.reported[read.op.Node] {
			continue
		}
		before := sort.Search(len(group.reads), func(i int) bool { return group.reads[i].op.Complete >= read.op.Invoke })
		if before == 0 {
			continue
		}
		witness := group.longest[before-1]
		limit, ok := thresholds[witness.n]
		if !ok {
			limit = insertionThreshold(earlier, witness.n, later)
			thresholds[witness.n] = limit
		}
		if limit.observed >= read.n {
			continue
		}
		report.add(witness, read, func() map[string]any {
			return map[string]any{
				"inserted_value":        later.values[limit.inserted],
				"observed_value":        later.values[limit.observed],
				"second_read_positions": []int{limit.inserted, limit.observed},
			}
		})
	}
}

func insertionThreshold(earlier *arena, n int, later *arena) insertion {
	result := insertion{math.MaxInt, math.MaxInt}
	for at, value := range later.values {
		position, ok := earlier.index(value)
		if !ok || position >= n {
			if result.inserted == math.MaxInt {
				result.inserted = at
			}
			continue
		}
		if result.inserted != math.MaxInt {
			result.observed = at
			return result
		}
	}
	return result
}

func describeRead(read *logRead) map[string]any {
	if read == nil {
		return nil
	}
	description := describe(read.op)
	description["length"] = read.n
	return description
}
