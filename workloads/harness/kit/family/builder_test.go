// SPDX-License-Identifier: AGPL-3.0-or-later

package family

import (
	"bytes"
	"encoding/json"
	"sort"
	"testing"

	"harmony.local/harness/kit/history"
)

type builder struct {
	t     *testing.T
	files map[string]*bytes.Buffer
	next  map[string]int64
}

func newBuilder(t *testing.T) *builder {
	return &builder{t: t, files: map[string]*bytes.Buffer{}, next: map[string]int64{}}
}

func (b *builder) line(file string, record history.Record, v any) {
	b.t.Helper()
	value, err := json.Marshal(v)
	if err != nil {
		b.t.Fatal(err)
	}
	record.V = value
	text, err := json.Marshal(record)
	if err != nil {
		b.t.Fatal(err)
	}
	if b.files[file] == nil {
		b.files[file] = &bytes.Buffer{}
	}
	b.files[file].Write(append(text, '\n'))
}

func (b *builder) op(file string, p int, f, k string, input any, invoke int64, outcome history.Type, output any, complete int64) int64 {
	return b.opOn(file, p, f, k, "", input, invoke, outcome, output, complete)
}

func (b *builder) opOn(file string, p int, f, k, node string, input any, invoke int64, outcome history.Type, output any, complete int64) int64 {
	index := b.next[file]
	b.next[file]++
	b.line(file, history.Record{P: p, T: history.Invoke, F: f, K: k, N: node, TS: invoke, I: index}, input)
	if outcome != "" {
		b.line(file, history.Record{P: p, T: outcome, F: f, K: k, TS: complete, I: index}, output)
	}
	return index
}

func (b *builder) build() *history.History {
	var files []history.File
	for name, data := range b.files {
		files = append(files, history.File{Name: name, Data: data.Bytes()})
	}
	return history.Build(files)
}

func kinds(report Report) []string {
	var found []string
	seen := map[string]bool{}
	for _, violation := range report.Violations {
		if !seen[violation.Kind] {
			seen[violation.Kind] = true
			found = append(found, violation.Kind)
		}
	}
	sort.Strings(found)
	return found
}

func expectKinds(t *testing.T, report Report, want ...string) {
	t.Helper()
	sort.Strings(want)
	got := kinds(report)
	if len(got) != len(want) {
		t.Fatalf("violations %v, want %v; details %+v", got, want, report.Violations)
	}
	for index := range got {
		if got[index] != want[index] {
			t.Fatalf("violations %v, want %v; details %+v", got, want, report.Violations)
		}
	}
}
