// SPDX-License-Identifier: AGPL-3.0-or-later

// Instrumented cooperative publish-before-initialize payload for M6.
package main

import (
	"encoding/binary"
	"errors"
	"io"
	"os"
)

const (
	maxFrame        = 4096
	headerLen       = 24
	serviceEvent    = 4
	serviceSDK      = 6
	resultEvent     = 0x06000001
	bootstrapThread = ^uint32(0)
)

type client struct {
	seq uint32
}

func (c *client) call(service uint16, opcode uint16, payload []byte) ([]byte, error) {
	if len(payload) > maxFrame-headerLen {
		return nil, errors.New("payload too large")
	}
	c.seq++
	request := make([]byte, headerLen+len(payload))
	copy(request[0:4], []byte("HCP1"))
	binary.LittleEndian.PutUint16(request[4:6], 1)
	binary.LittleEndian.PutUint16(request[6:8], service)
	binary.LittleEndian.PutUint16(request[8:10], opcode)
	binary.LittleEndian.PutUint32(request[12:16], c.seq)
	binary.LittleEndian.PutUint32(request[16:20], uint32(len(payload)))
	copy(request[headerLen:], payload)
	var length [4]byte
	binary.LittleEndian.PutUint32(length[:], uint32(len(request)))
	if _, err := os.Stdout.Write(length[:]); err != nil {
		return nil, err
	}
	if _, err := os.Stdout.Write(request); err != nil {
		return nil, err
	}
	if err := readFull(os.Stdin, length[:]); err != nil {
		return nil, err
	}
	responseLen := int(binary.LittleEndian.Uint32(length[:]))
	if responseLen < headerLen || responseLen > maxFrame {
		return nil, errors.New("invalid response length")
	}
	response := make([]byte, responseLen)
	if err := readFull(os.Stdin, response); err != nil {
		return nil, err
	}
	if string(response[0:4]) != "HCP1" ||
		binary.LittleEndian.Uint16(response[4:6]) != 2 ||
		binary.LittleEndian.Uint16(response[6:8]) != service ||
		binary.LittleEndian.Uint16(response[8:10]) != opcode ||
		binary.LittleEndian.Uint16(response[10:12]) != 0 ||
		binary.LittleEndian.Uint32(response[12:16]) != c.seq ||
		binary.LittleEndian.Uint32(response[20:24]) != 0 {
		return nil, errors.New("invalid response header")
	}
	payloadLen := int(binary.LittleEndian.Uint32(response[16:20]))
	if payloadLen != responseLen-headerLen {
		return nil, errors.New("invalid response payload length")
	}
	return response[headerLen:], nil
}

func readFull(reader io.Reader, buffer []byte) error {
	_, err := io.ReadFull(reader, buffer)
	return err
}

func (c *client) coverageYield(thread uint32, observed uint64, ready uint32) (uint32, error) {
	request := make([]byte, 16)
	binary.LittleEndian.PutUint32(request[0:4], thread)
	binary.LittleEndian.PutUint64(request[4:12], observed)
	binary.LittleEndian.PutUint32(request[12:16], ready)
	response, err := c.call(serviceSDK, 2, request)
	if err != nil {
		return 0, err
	}
	if len(response) != 12 || binary.LittleEndian.Uint64(response[0:8]) <= observed {
		return 0, errors.New("invalid coverage response")
	}
	selected := binary.LittleEndian.Uint32(response[8:12])
	if selected >= ready {
		return 0, errors.New("selected runnable out of range")
	}
	return selected, nil
}

func (c *client) result(bug bool, value byte) error {
	payload := make([]byte, 6)
	binary.LittleEndian.PutUint32(payload[0:4], resultEvent)
	if bug {
		payload[4] = 1
	}
	payload[5] = value
	response, err := c.call(serviceEvent, 1, payload)
	if err != nil {
		return err
	}
	if len(response) != 0 {
		return errors.New("event response was not empty")
	}
	return nil
}

type actor struct {
	step     byte
	observed uint64
}

type stepResult struct {
	actorID int
	step    byte
}

func actorLoop(
	actorID int,
	commands <-chan bool,
	completed chan<- stepResult,
	stopped chan<- int,
	published *bool,
	initialized *byte,
	consumerSawPublished *bool,
	observedValue *byte,
) {
	defer func() { stopped <- actorID }()
	step := byte(0)
	for run := range commands {
		if !run {
			return
		}
		if actorID == 0 {
			if step == 0 {
				*published = true
			} else {
				*initialized = 42
			}
		} else if step == 0 {
			*consumerSawPublished = *published
		} else if *consumerSawPublished {
			*observedValue = *initialized
		}
		step++
		completed <- stepResult{actorID: actorID, step: step}
	}
}

func runnable(actors *[2]actor) []int {
	ready := make([]int, 0, 2)
	for index := range actors {
		if actors[index].step < 2 {
			ready = append(ready, index)
		}
	}
	return ready
}

func run() error {
	c := client{}
	actors := [2]actor{}
	published := false
	initialized := byte(0)
	consumerSawPublished := false
	observedValue := byte(0xff)
	commands := [2]chan bool{make(chan bool), make(chan bool)}
	completed := make(chan stepResult)
	stopped := make(chan int, 2)
	for actorID := range commands {
		go actorLoop(
			actorID,
			commands[actorID],
			completed,
			stopped,
			&published,
			&initialized,
			&consumerSawPublished,
			&observedValue,
		)
	}
	selected, err := c.coverageYield(bootstrapThread, 1, 2)
	if err != nil {
		return err
	}
	for {
		ready := runnable(&actors)
		if int(selected) >= len(ready) {
			return errors.New("host selected no runnable actor")
		}
		actorID := ready[selected]
		commands[actorID] <- true
		result := <-completed
		if result.actorID != actorID || result.step != actors[actorID].step+1 {
			return errors.New("actor completion did not match the selected step")
		}
		a := &actors[actorID]
		a.step = result.step
		a.observed++
		ready = runnable(&actors)
		if len(ready) == 0 {
			break
		}
		selected, err = c.coverageYield(uint32(actorID), a.observed, uint32(len(ready)))
		if err != nil {
			return err
		}
	}
	for actorID := range commands {
		commands[actorID] <- false
		close(commands[actorID])
	}
	for range commands {
		<-stopped
	}
	return c.result(consumerSawPublished && observedValue == 0, observedValue)
}

func main() {
	if err := run(); err != nil {
		os.Stderr.WriteString(err.Error() + "\n")
		os.Exit(1)
	}
}
