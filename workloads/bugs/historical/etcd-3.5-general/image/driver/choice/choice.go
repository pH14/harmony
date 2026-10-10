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

// Bias returns the option the current action's choice prefers at a decision
// site, or -1 when the choice leaves the site to the stream. The choice holds
// one byte per site (site modulo 8): its top two bits say how often the site
// follows the choice (never, 1 in 2, 3 in 4 or 7 in 8 draws) and its low six
// bits name the preferred option, modulo n. The search decides which sites a
// choice biases and toward what.
func (s *Stream) Bias(site, n uint) int {
	if !s.folded || n == 0 {
		return -1
	}
	b := byte(s.last >> (8 * (site % 8)))
	follow := [4]uint64{0, 4, 6, 7}[b>>6]
	if follow == 0 || s.Below(8) >= follow {
		return -1
	}
	return int(uint(b&0x3f) % n)
}

// Pick draws an option at a decision site: the choice's preference when it
// applies, otherwise uniformly.
func (s *Stream) Pick(site, n uint) uint64 {
	if preferred := s.Bias(site, n); preferred >= 0 {
		return uint64(preferred)
	}
	return s.Below(uint64(n))
}

// ThinkAt pauses after an operation. Option 0 is no pause and option k is
// 2^(k-1) ms; unbiased, it pauses with probability one half for 1 to 128 ms.
func (s *Stream) ThinkAt(site uint) {
	level := s.Bias(site, 9)
	switch {
	case level < 0:
		s.Think()
	case level > 0:
		time.Sleep(time.Duration(1<<(level-1)) * time.Millisecond)
	}
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
