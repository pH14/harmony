// SPDX-License-Identifier: AGPL-3.0-or-later

package sdk

import (
	cryptorand "crypto/rand"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"math/rand/v2"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"sync"
	"unsafe"

	"golang.org/x/sys/unix"
)

const DevicePath = "/dev/harmony"

const commandEntropy = 0

const (
	ChoiceNamespace  = 11
	MaxChoiceSite    = 64
	MaxChoiceOptions = 1<<16 - 1

	exchangeRequest = 0xc0204801
	frameMagic      = 0x31504348
	frameHeader     = 24
	frameMax        = 4096
	kindRequest     = 1
	kindResponse    = 2
	serviceSDK      = 6
	opcodeService   = 3
)

var random struct {
	sync.Mutex
	device string
	native *rand.PCG
}

var choice struct {
	sync.Mutex
	exchange func(request []byte) ([]byte, error)
	seq      uint32
	asked    uint64
}

func init() {
	random.device = DevicePath
	choice.exchange = deviceExchange
}

func Random() uint64 {
	random.Lock()
	defer random.Unlock()
	if random.native == nil {
		value, err := deviceRandom(random.device)
		if err == nil {
			return value
		}
		if !errors.Is(err, fs.ErrNotExist) {
			panic("harmony entropy device: " + err.Error())
		}
		random.native = rand.NewPCG(nativeSeed())
	}
	return random.native.Uint64()
}

func Below(site string, n uint64) uint64 {
	if n == 0 {
		return 0
	}
	draw := Random() % n
	if n < 2 || n > MaxChoiceOptions || !hosted() {
		return draw
	}
	return uint64(ask(site, uint16(n), uint16(draw)))
}

func hosted() bool {
	random.Lock()
	defer random.Unlock()
	return random.native == nil
}

func ask(site string, options, draw uint16) uint16 {
	if site == "" || len(site) > MaxChoiceSite {
		panic(fmt.Sprintf("harmony choice site %q must have 1 to %d bytes", site, MaxChoiceSite))
	}
	choice.Lock()
	defer choice.Unlock()
	choice.seq++
	choice.asked++
	request := choiceRequest(choice.seq, uint64(os.Getpid())<<32|choice.asked, site, options, draw, monotonic())
	response, err := choice.exchange(request)
	if err != nil {
		panic("harmony choice exchange: " + err.Error())
	}
	label, err := choiceAnswer(response, choice.seq, options, draw)
	if err != nil {
		panic("harmony choice answer: " + err.Error())
	}
	return label
}

func monotonic() uint64 {
	var now unix.Timespec
	if err := unix.ClockGettime(unix.CLOCK_MONOTONIC, &now); err != nil {
		panic("monotonic clock: " + err.Error())
	}
	return uint64(now.Nano())
}

func choiceRequest(seq uint32, id uint64, site string, options, draw uint16, clock uint64) []byte {
	payload := make([]byte, 0, 22+len(site))
	payload = binary.LittleEndian.AppendUint16(payload, ChoiceNamespace)
	payload = binary.LittleEndian.AppendUint64(payload, id)
	payload = binary.LittleEndian.AppendUint16(payload, options)
	payload = binary.LittleEndian.AppendUint16(payload, draw)
	payload = binary.LittleEndian.AppendUint64(payload, clock)
	payload = append(payload, site...)
	frame := make([]byte, frameHeader, frameHeader+len(payload))
	binary.LittleEndian.PutUint32(frame[0:], frameMagic)
	binary.LittleEndian.PutUint16(frame[4:], kindRequest)
	binary.LittleEndian.PutUint16(frame[6:], serviceSDK)
	binary.LittleEndian.PutUint16(frame[8:], opcodeService)
	binary.LittleEndian.PutUint32(frame[12:], seq)
	binary.LittleEndian.PutUint32(frame[16:], uint32(len(payload)))
	return append(frame, payload...)
}

func choiceAnswer(frame []byte, seq uint32, options, draw uint16) (uint16, error) {
	if len(frame) < frameHeader {
		return 0, errors.New("response frame is shorter than its header")
	}
	if binary.LittleEndian.Uint32(frame[0:]) != frameMagic ||
		binary.LittleEndian.Uint16(frame[4:]) != kindResponse ||
		binary.LittleEndian.Uint16(frame[6:]) != serviceSDK ||
		binary.LittleEndian.Uint16(frame[8:]) != opcodeService ||
		binary.LittleEndian.Uint32(frame[12:]) != seq ||
		binary.LittleEndian.Uint32(frame[20:]) != 0 {
		return 0, errors.New("response header does not match its request")
	}
	if status := binary.LittleEndian.Uint16(frame[10:]); status != 0 {
		return 0, fmt.Errorf("host status %d", status)
	}
	payload := frame[frameHeader:]
	if int(binary.LittleEndian.Uint32(frame[16:])) != len(payload) {
		return 0, errors.New("response length does not match its frame")
	}
	switch {
	case len(payload) == 1 && payload[0] == 0:
		return draw, nil
	case len(payload) == 3 && payload[0] == 1:
		label := binary.LittleEndian.Uint16(payload[1:])
		if label >= options {
			return 0, fmt.Errorf("label %d is outside %d options", label, options)
		}
		return label, nil
	}
	return 0, errors.New("malformed choice answer")
}

type exchangeArgs struct {
	request          uint64
	response         uint64
	requestLen       uint32
	responseCapacity uint32
	responseLen      uint32
	reserved         uint32
}

// The ioctl argument carries raw buffer addresses, so the buffers are package
// variables, which the Go runtime never moves.
var exchangeBuffers struct {
	request  [frameMax]byte
	response [frameMax]byte
}

func deviceExchange(request []byte) ([]byte, error) {
	if len(request) > frameMax {
		return nil, errors.New("choice request exceeds one frame")
	}
	fd, err := unix.Open(DevicePath, unix.O_RDWR|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	defer unix.Close(fd)
	copy(exchangeBuffers.request[:], request)
	args := exchangeArgs{
		request:          uint64(uintptr(unsafe.Pointer(&exchangeBuffers.request))),
		response:         uint64(uintptr(unsafe.Pointer(&exchangeBuffers.response))),
		requestLen:       uint32(len(request)),
		responseCapacity: frameMax,
	}
	if _, _, errno := unix.Syscall(unix.SYS_IOCTL, uintptr(fd), exchangeRequest, uintptr(unsafe.Pointer(&args))); errno != 0 {
		return nil, errno
	}
	if args.responseLen > frameMax {
		return nil, errors.New("choice response exceeds one frame")
	}
	return append([]byte(nil), exchangeBuffers.response[:args.responseLen]...), nil
}

func deviceRandom(path string) (uint64, error) {
	device, err := os.OpenFile(path, os.O_RDWR, 0)
	if err != nil {
		return 0, err
	}
	defer device.Close()
	if _, err := device.Write([]byte{commandEntropy}); err != nil {
		return 0, err
	}
	var bytes [8]byte
	if _, err := io.ReadFull(device, bytes[:]); err != nil {
		return 0, err
	}
	return binary.LittleEndian.Uint64(bytes[:]), nil
}

func nativeSeed() (uint64, uint64) {
	if text := os.Getenv("HARNESS_SEED"); text != "" {
		if seed, err := strconv.ParseUint(text, 0, 64); err == nil {
			return seed, seed ^ 0x9e3779b97f4a7c15
		}
	}
	var bytes [16]byte
	if _, err := cryptorand.Read(bytes[:]); err != nil {
		panic("seed native random stream: " + err.Error())
	}
	return binary.LittleEndian.Uint64(bytes[:8]), binary.LittleEndian.Uint64(bytes[8:])
}

type Kind int

const (
	KindAlways Kind = iota
	KindSometimes
	KindReachable
	KindUnreachable
)

func (k Kind) record() (assertType, display string, mustHit bool) {
	switch k {
	case KindSometimes:
		return "sometimes", "Sometimes", true
	case KindReachable:
		return "reachability", "Reachable", true
	case KindUnreachable:
		return "reachability", "Unreachable", false
	default:
		return "always", "Always", true
	}
}

var sink sync.Mutex

func Always(cond bool, message string, details map[string]any) {
	emit(KindAlways, true, cond, message, details, 2)
}

func Sometimes(cond bool, message string, details map[string]any) {
	emit(KindSometimes, true, cond, message, details, 2)
}

func Reachable(message string, details map[string]any) {
	emit(KindReachable, true, true, message, details, 2)
}

func Unreachable(message string, details map[string]any) {
	emit(KindUnreachable, true, false, message, details, 2)
}

func Declare(kind Kind, message string) {
	emit(kind, false, false, message, nil, 2)
}

func emit(kind Kind, hit, cond bool, message string, details map[string]any, skip int) {
	dir := os.Getenv("ANTITHESIS_OUTPUT_DIR")
	if dir == "" {
		return
	}
	line, err := Record(kind, hit, cond, message, details, caller(skip+1))
	if err != nil {
		panic("encode assertion: " + err.Error())
	}
	write(dir, line)
}

func CheckUndecided(keys int) {
	dir := os.Getenv("ANTITHESIS_OUTPUT_DIR")
	if dir == "" || keys <= 0 {
		return
	}
	write(dir, UndecidedRecord(keys))
}

func UndecidedRecord(keys int) []byte {
	return fmt.Appendf(nil, "{\"harmony_check\":{\"undecided_keys\":%d}}\n", keys)
}

func write(dir string, line []byte) {
	sink.Lock()
	defer sink.Unlock()
	file, err := os.OpenFile(filepath.Join(dir, "sdk.jsonl"), os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		panic("open assertion sink: " + err.Error())
	}
	_, writeErr := file.Write(line)
	closeErr := file.Close()
	if writeErr != nil {
		panic("write assertion: " + writeErr.Error())
	}
	if closeErr != nil {
		panic("close assertion sink: " + closeErr.Error())
	}
}

type Location struct {
	Function string
	File     string
	Line     int
}

func caller(skip int) Location {
	pc, file, line, ok := runtime.Caller(skip)
	if !ok {
		return Location{}
	}
	name := ""
	if fn := runtime.FuncForPC(pc); fn != nil {
		name = fn.Name()
	}
	return Location{Function: name, File: file, Line: line}
}

func Record(kind Kind, hit, cond bool, message string, details map[string]any, at Location) ([]byte, error) {
	assertType, display, mustHit := kind.record()
	if kind == KindReachable || kind == KindUnreachable {
		cond = hit
	}
	var encodedDetails any
	if len(details) > 0 {
		encodedDetails = details
	}
	line, err := json.Marshal(map[string]any{
		"antithesis_assert": map[string]any{
			"hit":          hit,
			"must_hit":     mustHit,
			"assert_type":  assertType,
			"display_type": display,
			"message":      message,
			"condition":    cond,
			"id":           message,
			"location": map[string]any{
				"class":        "",
				"function":     at.Function,
				"file":         at.File,
				"begin_line":   at.Line,
				"begin_column": 0,
			},
			"details": encodedDetails,
		},
	})
	if err != nil {
		return nil, err
	}
	return append(line, '\n'), nil
}
