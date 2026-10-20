// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const (
	streamName        = "S"
	subject           = "s.x"
	readBatch         = 1024
	fencePrefix       = "fence-"
	placementAttempts = 16
)

var servers = []string{
	"nats://10.77.0.1:4222",
	"nats://10.77.0.2:4222",
	"nats://10.77.0.3:4222",
}

var nodes = []string{"n1", "n2", "n3"}

var streamConfig = jetstream.StreamConfig{
	Name:     streamName,
	Subjects: []string{subject},
	Replicas: 3,
	Storage:  jetstream.FileStorage,
}

type adapter struct{}

func (adapter) Family() family.Family { return family.Log{} }

func (adapter) Nodes() []string { return nodes }

type client struct {
	conn *nats.Conn
	js   jetstream.JetStream
}

func dial(process int) (*client, error) {
	first := int(sdk.Below(uint64(len(servers))))
	ordered := append(append([]string(nil), servers[first:]...), servers[:first]...)
	conn, err := nats.Connect(strings.Join(ordered, ","),
		nats.Name(fmt.Sprintf("harness-%d", process)),
		nats.DontRandomize(),
		nats.Timeout(time.Second),
		nats.MaxReconnects(-1),
		nats.ReconnectWait(100*time.Millisecond),
		nats.ReconnectJitter(0, 0),
		nats.ReconnectBufSize(-1),
		nats.PingInterval(time.Second),
		nats.MaxPingsOutstanding(3),
		nats.NoCallbacksAfterClientClose(),
	)
	if err != nil {
		return nil, err
	}
	js, err := jetstream.New(conn)
	if err != nil {
		conn.Close()
		return nil, err
	}
	return &client{conn: conn, js: js}, nil
}

func (adapter) Connect(_ context.Context, process int) (runner.Client, error) {
	return dial(process)
}

func (c *client) Close() { c.conn.Close() }

func (c *client) Invoke(ctx context.Context, op family.Op) (any, error) {
	switch op.F {
	case family.LogAppend:
		ack, err := c.js.Publish(ctx, subject, []byte(op.V.(string)),
			jetstream.WithRetryAttempts(0),
			jetstream.WithExpectStream(streamName))
		if unsent(err) {
			return nil, runner.Definite(err)
		}
		if err != nil {
			return nil, err
		}
		return ack.Sequence, nil
	case family.LogRead:
		read := c.read
		if op.Node != "" {
			read = func(ctx context.Context) ([]string, error) { return c.readOn(ctx, op.Node) }
		}
		values, err := read(ctx)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return values, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

// nats.go returns these from publish before the message is buffered, so the
// server never saw it.
func unsent(err error) bool {
	return errors.Is(err, nats.ErrReconnectBufExceeded) || errors.Is(err, nats.ErrConnectionDraining)
}

func (c *client) fence(ctx context.Context) (uint64, error) {
	fence := fmt.Sprintf("%s%016x", fencePrefix, sdk.Random())
	ack, err := c.js.Publish(ctx, subject, []byte(fence),
		jetstream.WithRetryAttempts(0),
		jetstream.WithExpectStream(streamName))
	if err != nil {
		return 0, err
	}
	return ack.Sequence, nil
}

func (c *client) read(ctx context.Context) ([]string, error) {
	last, err := c.fence(ctx)
	if err != nil {
		if c.streamAbsent(ctx) {
			return []string{}, nil
		}
		return nil, fmt.Errorf("fence: %w", err)
	}
	values := []string{}
	keep := func(data []byte) {
		if value := string(data); !strings.HasPrefix(value, fencePrefix) {
			values = append(values, value)
		}
	}
	seen := uint64(0)
	var stream jetstream.Stream
	var consumer jetstream.Consumer
	defer func() {
		if consumer != nil {
			c.dropConsumer(consumer)
		}
	}()
	for seen < last {
		if ctx.Err() != nil {
			return nil, fmt.Errorf("fetched through %d of %d: %w", seen, last, ctx.Err())
		}
		if consumer == nil {
			consumer, err = c.js.CreateConsumer(ctx, streamName, jetstream.ConsumerConfig{
				DeliverPolicy:     jetstream.DeliverByStartSequencePolicy,
				OptStartSeq:       seen + 1,
				AckPolicy:         jetstream.AckNonePolicy,
				MemoryStorage:     true,
				Replicas:          1,
				InactiveThreshold: 5 * time.Second,
			})
			if err != nil {
				return nil, fmt.Errorf("consumer at %d of %d: %w", seen+1, last, err)
			}
		}
		msgs, err := fetch(ctx, consumer, int(min(last-seen, readBatch)))
		if err != nil {
			return nil, fmt.Errorf("fetch after %d of %d: %w", seen, last, err)
		}
		for _, msg := range msgs {
			meta, err := msg.Metadata()
			if err != nil {
				return nil, err
			}
			sequence := meta.Sequence.Stream
			if sequence <= seen {
				continue
			}
			if sequence == seen+1 {
				keep(msg.Data())
				seen = sequence
				continue
			}
			// A consumer without acks drops a batch the server sends after the
			// fetch expired, so the stream decides whether the sequence exists.
			c.dropConsumer(consumer)
			consumer = nil
			if stream == nil {
				if stream, err = c.js.Stream(ctx, streamName); err != nil {
					return nil, fmt.Errorf("stream at %d of %d: %w", seen+1, last, err)
				}
			}
			raw, err := stream.GetMsg(ctx, seen+1)
			switch {
			case errors.Is(err, jetstream.ErrMsgNotFound):
			case err != nil:
				return nil, fmt.Errorf("message %d of %d: %w", seen+1, last, err)
			default:
				keep(raw.Data)
			}
			seen++
			break
		}
	}
	return values, nil
}

// A one-replica consumer serves messages from the store of the server that
// hosts it, so a read through consumers on one node sees only that node's
// copy of the stream. Sequences the node lacks are skipped.
func (c *client) readOn(ctx context.Context, node string) ([]string, error) {
	last, err := c.fence(ctx)
	if err != nil {
		return nil, fmt.Errorf("fence: %w", err)
	}
	values := []string{}
	seen := uint64(0)
	delivered := uint64(0)
	var consumer jetstream.Consumer
	defer func() {
		if consumer != nil {
			c.dropConsumer(consumer)
		}
	}()
	for seen < last {
		if ctx.Err() != nil {
			return nil, fmt.Errorf("fetched through %d of %d on %s: %w", seen, last, node, ctx.Err())
		}
		if consumer == nil {
			consumer, err = c.consumerOn(ctx, node, seen+1)
			if err != nil {
				return nil, err
			}
			delivered = 0
		}
		msgs, err := fetch(ctx, consumer, int(min(last-seen, readBatch)))
		if err != nil {
			return nil, fmt.Errorf("fetch after %d of %d on %s: %w", seen, last, node, err)
		}
		for _, msg := range msgs {
			meta, err := msg.Metadata()
			if err != nil {
				return nil, err
			}
			// A jump in the consumer sequence means the client dropped a
			// delivery that arrived after its fetch expired.
			if meta.Sequence.Consumer != delivered+1 {
				c.dropConsumer(consumer)
				consumer = nil
				break
			}
			delivered++
			sequence := meta.Sequence.Stream
			if sequence <= seen {
				continue
			}
			if value := string(msg.Data()); sequence <= last && !strings.HasPrefix(value, fencePrefix) {
				values = append(values, value)
			}
			seen = sequence
		}
	}
	return values, nil
}

func (c *client) consumerOn(ctx context.Context, node string, start uint64) (jetstream.Consumer, error) {
	var placed []string
	for attempt := 0; attempt < placementAttempts; attempt++ {
		consumer, err := c.js.CreateConsumer(ctx, streamName, jetstream.ConsumerConfig{
			DeliverPolicy:     jetstream.DeliverByStartSequencePolicy,
			OptStartSeq:       start,
			AckPolicy:         jetstream.AckNonePolicy,
			MemoryStorage:     true,
			Replicas:          1,
			InactiveThreshold: 5 * time.Second,
		})
		if err != nil {
			return nil, fmt.Errorf("consumer at %d on %s: %w", start, node, err)
		}
		info := consumer.CachedInfo()
		if info.Cluster != nil && info.Cluster.Leader == node {
			return consumer, nil
		}
		c.dropConsumer(consumer)
		if info.Cluster != nil {
			placed = append(placed, info.Cluster.Leader)
		}
	}
	return nil, fmt.Errorf("no consumer placed on %s in %d attempts, placed on %v", node, placementAttempts, placed)
}

// A server that has not finished recovering can briefly deny the stream, so
// absence must be the answer twice.
func (c *client) streamAbsent(ctx context.Context) bool {
	for attempt := 0; attempt < 2; attempt++ {
		if attempt > 0 {
			select {
			case <-ctx.Done():
				return false
			case <-time.After(500 * time.Millisecond):
			}
		}
		if _, err := c.js.Stream(ctx, streamName); !errors.Is(err, jetstream.ErrStreamNotFound) {
			return false
		}
	}
	return true
}

func (c *client) dropConsumer(consumer jetstream.Consumer) {
	go func() {
		ctx, cancel := context.WithTimeout(context.Background(), time.Second)
		defer cancel()
		_ = c.js.DeleteConsumer(ctx, streamName, consumer.CachedInfo().Name)
	}()
}

func fetch(ctx context.Context, consumer jetstream.Consumer, count int) ([]jetstream.Msg, error) {
	ctx, cancel := context.WithTimeout(ctx, time.Second)
	defer cancel()
	batch, err := consumer.Fetch(count, jetstream.FetchContext(ctx))
	if err != nil {
		return nil, err
	}
	var msgs []jetstream.Msg
	for msg := range batch.Messages() {
		msgs = append(msgs, msg)
	}
	if err := batch.Error(); err != nil && !errors.Is(err, nats.ErrTimeout) && !errors.Is(err, context.DeadlineExceeded) {
		return nil, err
	}
	return msgs, nil
}

func (adapter) Ready(ctx context.Context) error {
	c, err := dial(0)
	if err != nil {
		return err
	}
	defer c.Close()
	if _, err := c.js.AccountInfo(ctx); err != nil {
		return err
	}
	stream, err := c.js.CreateStream(ctx, streamConfig)
	if err != nil {
		return err
	}
	info := stream.CachedInfo()
	if info.Config.Replicas != streamConfig.Replicas {
		return fmt.Errorf("stream %s has %d replicas", streamName, info.Config.Replicas)
	}
	if info.Cluster == nil || info.Cluster.Leader == "" {
		return fmt.Errorf("stream %s has no leader", streamName)
	}
	return nil
}

type streamState struct {
	Present  bool   `json:"present"`
	Messages uint64 `json:"messages,omitempty"`
	First    uint64 `json:"first,omitempty"`
	Last     uint64 `json:"last,omitempty"`
	Leader   string `json:"leader,omitempty"`
	Answers  int    `json:"answers"`
	Error    string `json:"error,omitempty"`
}

func probeStream(ctx context.Context) (streamState, error) {
	c, err := dial(0)
	if err != nil {
		return streamState{}, err
	}
	defer c.Close()
	ctx, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()
	stream, err := c.js.Stream(ctx, streamName)
	if errors.Is(err, jetstream.ErrStreamNotFound) {
		return streamState{}, nil
	}
	if err != nil {
		return streamState{}, err
	}
	info := stream.CachedInfo()
	state := streamState{Present: true, Messages: info.State.Msgs, First: info.State.FirstSeq, Last: info.State.LastSeq}
	if info.Cluster != nil {
		state.Leader = info.Cluster.Leader
	}
	return state, nil
}

func reportStream() {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	var result streamState
	for ctx.Err() == nil && result.Answers < 5 {
		state, err := probeStream(ctx)
		if err != nil {
			result.Error = err.Error()
		} else {
			state.Answers = result.Answers + 1
			if state.Present {
				result = state
				break
			}
			result = state
		}
		time.Sleep(time.Second)
	}
	line, _ := json.Marshal(result)
	fmt.Println(string(line))
	if result.Answers == 0 {
		os.Exit(1)
	}
}

func main() {
	if len(os.Args) > 1 && os.Args[1] == "stream" {
		reportStream()
		return
	}
	runner.Main(adapter{})
}
