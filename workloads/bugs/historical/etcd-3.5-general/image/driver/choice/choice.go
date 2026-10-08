// SPDX-License-Identifier: AGPL-3.0-or-later

// Package choice is the Go side of the general-discovery choice stream and
// Antithesis fallback SDK records. See workloads/bugs/historical/general.
package choice

import (
	"crypto/rand"
	"encoding/binary"
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"sync/atomic"
	"syscall"
	"time"
	"unsafe"
)

const (
	ioctlExchange   = 0xc0204801
	frameMagic      = 0x31504348
	serviceSDK      = 6
	serviceRequest  = 3
	choiceNamespace = 11
	headerLen       = 24
)

type exchange struct {
	request          uint64
	response         uint64
	requestLen       uint32
	responseCapacity uint32
	responseLen      uint32
	reserved         uint32
}

var sequence atomic.Uint32

// Request is the SDK opaque service frame that asks for the current action
// choice.
func Request(seq uint32) []byte {
	frame := make([]byte, headerLen+10)
	binary.LittleEndian.PutUint32(frame[0:], frameMagic)
	binary.LittleEndian.PutUint16(frame[4:], 1)
	binary.LittleEndian.PutUint16(frame[6:], serviceSDK)
	binary.LittleEndian.PutUint16(frame[8:], serviceRequest)
	binary.LittleEndian.PutUint32(frame[12:], seq)
	binary.LittleEndian.PutUint32(frame[16:], 10)
	binary.LittleEndian.PutUint16(frame[headerLen:], choiceNamespace)
	return frame
}

// Parse returns the choice in a response frame, or false when the host did
// not answer one.
func Parse(response []byte) (uint64, bool) {
	if len(response) != headerLen+9 {
		return 0, false
	}
	if binary.LittleEndian.Uint32(response[0:]) != frameMagic ||
		binary.LittleEndian.Uint16(response[10:]) != 0 ||
		binary.LittleEndian.Uint32(response[16:]) != 9 ||
		response[headerLen] != 1 {
		return 0, false
	}
	return binary.LittleEndian.Uint64(response[headerLen+1:]), true
}

// Stream is one client's splitmix64 state with the current action choice
// folded in before each operation.
type Stream struct {
	state  uint64
	last   uint64
	folded bool
	device *os.File
}

func mix(z uint64) uint64 {
	z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9
	z = (z ^ (z >> 27)) * 0x94d049bb133111eb
	return z ^ (z >> 31)
}

func New() *Stream {
	var seed [8]byte
	if _, err := rand.Read(seed[:]); err != nil {
		binary.LittleEndian.PutUint64(seed[:], uint64(time.Now().UnixNano()))
	}
	stream := &Stream{state: binary.LittleEndian.Uint64(seed[:])}
	if device, err := os.OpenFile("/dev/harmony", os.O_RDWR, 0); err == nil {
		stream.device = device
	}
	return stream
}

// NewSeeded is a stream without a device, for tests.
func NewSeeded(seed uint64) *Stream { return &Stream{state: seed} }

func (s *Stream) query() (uint64, bool) {
	if s.device == nil {
		return 0, false
	}
	request := Request(sequence.Add(1))
	response := make([]byte, headerLen+16)
	ex := &exchange{
		request:          uint64(uintptr(unsafe.Pointer(&request[0]))),
		response:         uint64(uintptr(unsafe.Pointer(&response[0]))),
		requestLen:       uint32(len(request)),
		responseCapacity: uint32(len(response)),
	}
	_, _, errno := syscall.Syscall(syscall.SYS_IOCTL, s.device.Fd(), ioctlExchange, uintptr(unsafe.Pointer(ex)))
	runtime.KeepAlive(request)
	runtime.KeepAlive(response)
	if errno != 0 || int(ex.responseLen) > len(response) {
		return 0, false
	}
	return Parse(response[:ex.responseLen])
}

// Fold mixes a choice into the stream when it differs from the last one.
func (s *Stream) Fold(choice uint64) {
	if s.folded && choice == s.last {
		return
	}
	s.state ^= mix(choice ^ 0x6a09e667f3bcc909)
	s.last = choice
	s.folded = true
}

// Sync folds the current action's choice before an operation.
func (s *Stream) Sync() {
	if choice, ok := s.query(); ok {
		s.Fold(choice)
	}
}

func (s *Stream) Next() uint64 {
	s.state += 0x9e3779b97f4a7c15
	return mix(s.state)
}

func (s *Stream) Below(n uint64) uint64 {
	if n == 0 {
		return 0
	}
	return s.Next() % n
}

// Think pauses with probability one half for 1 to 128 ms.
func (s *Stream) Think() {
	if s.Below(2) == 0 {
		time.Sleep(time.Duration(1<<s.Below(8)) * time.Millisecond)
	}
}

// Assert writes one Antithesis fallback SDK record with one write call.
func Assert(assertType, id string, hit, condition bool, detail string) {
	dir := os.Getenv("ANTITHESIS_OUTPUT_DIR")
	if dir == "" {
		return
	}
	display := map[string]string{"always": "Always", "reachability": "Reachable"}[assertType]
	var details any
	if detail != "" {
		details = map[string]string{"detail": detail}
	}
	line, err := json.Marshal(map[string]any{
		"antithesis_assert": map[string]any{
			"hit":          hit,
			"must_hit":     true,
			"assert_type":  assertType,
			"display_type": display,
			"message":      id,
			"condition":    condition,
			"id":           id,
			"location": map[string]any{
				"class": "general", "function": "", "file": "",
				"begin_line": 0, "begin_column": 0,
			},
			"details": details,
		},
	})
	if err != nil {
		return
	}
	sink, err := os.OpenFile(filepath.Join(dir, "sdk.jsonl"), os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		return
	}
	_, _ = sink.Write(append(line, '\n'))
	_ = sink.Close()
}

func DeclareAlways(id string)    { Assert("always", id, false, false, "") }
func DeclareReachable(id string) { Assert("reachability", id, false, false, "") }
func Always(id string, condition bool, detail string) {
	if condition {
		detail = ""
	}
	Assert("always", id, true, condition, detail)
}
func Reached(id string) { Assert("reachability", id, true, true, "") }
