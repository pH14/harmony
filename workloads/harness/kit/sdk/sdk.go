// SPDX-License-Identifier: AGPL-3.0-or-later

package sdk

import (
	cryptorand "crypto/rand"
	"encoding/binary"
	"encoding/json"
	"errors"
	"io"
	"io/fs"
	"math/rand/v2"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"sync"
)

const DevicePath = "/dev/harmony"

const commandEntropy = 0

var random struct {
	sync.Mutex
	device string
	native *rand.PCG
}

func init() {
	random.device = DevicePath
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

func Below(n uint64) uint64 {
	if n == 0 {
		return 0
	}
	return Random() % n
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
