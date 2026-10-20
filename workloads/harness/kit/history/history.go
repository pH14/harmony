// SPDX-License-Identifier: AGPL-3.0-or-later

package history

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"

	"golang.org/x/sys/unix"
)

type Type string

const (
	Invoke Type = "invoke"
	OK     Type = "ok"
	Fail   Type = "fail"
	Info   Type = "info"
)

const Forever int64 = math.MaxInt64

const Suffix = ".jsonl"

type Record struct {
	P  int             `json:"p"`
	T  Type            `json:"t"`
	F  string          `json:"f"`
	K  string          `json:"k"`
	V  json.RawMessage `json:"v"`
	TS int64           `json:"ts"`
	I  int64           `json:"i"`
}

func Now() int64 {
	var now unix.Timespec
	if err := unix.ClockGettime(unix.CLOCK_MONOTONIC, &now); err != nil {
		panic("monotonic clock: " + err.Error())
	}
	return now.Nano()
}

type Journal struct {
	mu    sync.Mutex
	file  *os.File
	name  string
	token uint64
	next  int64
}

func Create(dir, prefix string, token func() uint64) (*Journal, error) {
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return nil, err
	}
	for {
		drawn := token()
		name := fmt.Sprintf("%s-%016x%s", prefix, drawn, Suffix)
		file, err := os.OpenFile(filepath.Join(dir, name), os.O_CREATE|os.O_EXCL|os.O_WRONLY|os.O_APPEND, 0o644)
		if errors.Is(err, os.ErrExist) {
			continue
		}
		if err != nil {
			return nil, err
		}
		return &Journal{file: file, name: name, token: drawn}, nil
	}
}

func (j *Journal) Name() string {
	return j.name
}

func (j *Journal) Token() uint64 {
	return j.token
}

func (j *Journal) Invoke(p int, f, k string, v any) (int64, error) {
	j.mu.Lock()
	defer j.mu.Unlock()
	index := j.next
	j.next++
	return index, j.write(Record{P: p, T: Invoke, F: f, K: k, I: index}, v)
}

func (j *Journal) Complete(p int, t Type, f, k string, index int64, v any) error {
	j.mu.Lock()
	defer j.mu.Unlock()
	return j.write(Record{P: p, T: t, F: f, K: k, I: index}, v)
}

func (j *Journal) write(record Record, v any) error {
	value, err := json.Marshal(v)
	if err != nil {
		return err
	}
	record.V = value
	record.TS = Now()
	line, err := json.Marshal(record)
	if err != nil {
		return err
	}
	line = append(line, '\n')
	n, err := j.file.Write(line)
	if err == nil && n != len(line) {
		err = fmt.Errorf("journal %s: short write", j.name)
	}
	return err
}

func (j *Journal) Close() error {
	return j.file.Close()
}

type Op struct {
	Process  int
	Journal  string
	P        int
	Index    int64
	F        string
	K        string
	Input    json.RawMessage
	Output   json.RawMessage
	Type     Type
	Invoke   int64
	Complete int64
}

func (op *Op) Definite() bool {
	return op.Type == OK || op.Type == Fail
}

type Ref struct {
	Journal string `json:"j"`
	Index   int64  `json:"i"`
}

type History struct {
	Ops       []Op
	Files     int
	Malformed int
	byRef     map[Ref]int
}

func (h *History) Lookup(ref Ref) (*Op, bool) {
	at, ok := h.byRef[ref]
	if !ok {
		return nil, false
	}
	return &h.Ops[at], true
}

func Load(dir string) (*History, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return Build(nil), nil
		}
		return nil, err
	}
	var files []File
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), Suffix) {
			continue
		}
		data, err := os.ReadFile(filepath.Join(dir, entry.Name()))
		if err != nil {
			if errors.Is(err, os.ErrNotExist) {
				continue
			}
			return nil, err
		}
		files = append(files, File{Name: entry.Name(), Data: data})
	}
	return Build(files), nil
}

type File struct {
	Name string
	Data []byte
}

func Build(files []File) *History {
	sort.Slice(files, func(i, j int) bool { return files[i].Name < files[j].Name })
	h := &History{Files: len(files), byRef: map[Ref]int{}}
	type process struct {
		journal string
		p       int
	}
	processes := map[process]int{}
	for _, file := range files {
		data := file.Data
		if end := bytes.LastIndexByte(data, '\n'); end+1 < len(data) {
			data = data[:end+1]
		}
		pending := map[int64]int{}
		for _, line := range bytes.Split(data, []byte{'\n'}) {
			if len(line) == 0 {
				continue
			}
			var record Record
			if err := json.Unmarshal(line, &record); err != nil {
				h.Malformed++
				continue
			}
			ref := Ref{Journal: file.Name, Index: record.I}
			switch record.T {
			case Invoke:
				if _, seen := h.byRef[ref]; seen {
					h.Malformed++
					continue
				}
				key := process{file.Name, record.P}
				id, ok := processes[key]
				if !ok {
					id = len(processes)
					processes[key] = id
				}
				h.byRef[ref] = len(h.Ops)
				pending[record.I] = len(h.Ops)
				h.Ops = append(h.Ops, Op{
					Process:  id,
					Journal:  file.Name,
					P:        record.P,
					Index:    record.I,
					F:        record.F,
					K:        record.K,
					Input:    record.V,
					Type:     Info,
					Invoke:   record.TS,
					Complete: Forever,
				})
			case OK, Fail, Info:
				at, ok := pending[record.I]
				if !ok {
					h.Malformed++
					continue
				}
				delete(pending, record.I)
				op := &h.Ops[at]
				op.Type = record.T
				op.Output = record.V
				if record.T != Info {
					op.Complete = record.TS
				}
			default:
				h.Malformed++
			}
		}
	}
	order := make([]int, len(h.Ops))
	for i := range order {
		order[i] = i
	}
	sort.SliceStable(order, func(a, b int) bool {
		return h.Ops[order[a]].Invoke < h.Ops[order[b]].Invoke
	})
	sorted := make([]Op, len(h.Ops))
	for to, from := range order {
		sorted[to] = h.Ops[from]
	}
	h.Ops = sorted
	for at, op := range h.Ops {
		h.byRef[Ref{Journal: op.Journal, Index: op.Index}] = at
	}
	return h
}
