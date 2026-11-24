// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
	"harmony.local/harness/kit/sdk"
)

const (
	membersEnv    = "HARNESS_ETCD_MEMBERS"
	watchEnv      = "HARNESS_ETCD_WATCH"
	defragmentEnv = "HARNESS_ETCD_DEFRAGMENT"
	writesEnv     = "HARNESS_ETCD_WRITES_ONLY"
	keepAliveEnv  = "HARNESS_ETCD_KEEPALIVE"

	defragmentLimit = 2 * time.Second
)

const (
	watchFirst = "k"
	watchEnd   = "l"
	watchSpare = 250 * time.Millisecond
)

type adapter struct {
	members    []string
	names      []string
	watch      uint64
	defragment uint64
	writesOnly bool
	keepAlive  bool
}

func (a adapter) Family() family.Family {
	return family.Register{NoReads: a.writesOnly, Watch: a.watch}
}

func (a adapter) Nodes() []string { return a.names }

func (a adapter) Connect(context.Context, int) (runner.Client, error) {
	return client{members: a.members, names: a.names, defragment: a.defragment, http: newHTTP(a.keepAlive)}, nil
}

type client struct {
	members    []string
	names      []string
	defragment uint64
	http       *http.Client
}

func newHTTP(keepAlive bool) *http.Client {
	return &http.Client{Transport: &http.Transport{
		Proxy:               nil,
		DialContext:         (&net.Dialer{Timeout: time.Second}).DialContext,
		DisableKeepAlives:   !keepAlive,
		MaxIdleConnsPerHost: 1,
	}}
}

func (c client) Close() { c.http.CloseIdleConnections() }

type kv struct {
	Key            string `json:"key"`
	Value          string `json:"value"`
	CreateRevision number `json:"create_revision"`
	ModRevision    number `json:"mod_revision"`
	Version        number `json:"version"`
}

// The gateway writes 64-bit integers as JSON strings.
type number uint64

func (c *number) UnmarshalJSON(data []byte) error {
	n, err := strconv.ParseUint(strings.Trim(string(data), `"`), 10, 64)
	*c = number(n)
	return err
}

// The gateway omits zero fields: a range with no match has no "kvs" and a
// transaction whose compare failed has no "succeeded".
type reply struct {
	Header    header `json:"header"`
	Kvs       []kv   `json:"kvs"`
	Succeeded bool   `json:"succeeded"`
	Error     string `json:"error"`
	Message   string `json:"message"`
}

type header struct {
	Revision number `json:"revision"`
}

const revisionNeverFalls = "etcd: a linearizable request's response revision is never below a revision acknowledged before the request was sent"

// A put, the compare-and-swap transaction and a range without "serializable"
// go through the raft log or a read index, so the member answers from a store
// that holds every write acknowledged before the request was sent.
// Serializable ranges answer from the member's own store and are exempt. The
// floor is the highest revision this process has seen acknowledged when it
// starts the request; each harness process (ready, drive, check) starts from
// zero.
type revisions struct {
	mu     sync.Mutex
	high   uint64
	from   string
	assert func(bool, string, map[string]any)
}

var acknowledged = &revisions{assert: sdk.Always}

func linearizable(path string, body any) bool {
	fields, ok := body.(map[string]any)
	if !ok {
		return false
	}
	if _, set := fields["serializable"]; set {
		return false
	}
	return path == "/v3/kv/put" || path == "/v3/kv/txn" || path == "/v3/kv/range"
}

func (r *revisions) floor() (uint64, string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	return r.high, r.from
}

func (r *revisions) answered(member, path string, floor uint64, from string, got uint64) {
	r.assert(got >= floor, revisionNeverFalls, map[string]any{
		"member": member, "path": path, "revision": got, "acknowledged": floor, "acknowledged_by": from,
	})
	r.mu.Lock()
	defer r.mu.Unlock()
	if got > r.high {
		r.high, r.from = got, member
	}
}

func enc(s string) string { return base64.StdEncoding.EncodeToString([]byte(s)) }

// Each operation goes to one member drawn from the SDK stream, so reads and
// writes reach followers as well as the leader. With defragment set to N, one
// operation in N first defragments that member's backend; the result is not
// recorded because a defragment changes no key. The defragment gets at most
// half of the operation's remaining time, and an operation whose deadline
// passed during the defragment fails definitely because its request was never
// sent.
func (c client) Invoke(ctx context.Context, op family.Op) (any, error) {
	base := c.members[sdk.Below("etcd.server", uint64(len(c.members)))]
	if c.defragment > 0 && sdk.Below("etcd.defragment", c.defragment) == 0 {
		c.defragmentMember(ctx, base)
		if err := expired(ctx); err != nil {
			return nil, runner.Definite(fmt.Errorf("defragment of %s: %w", base, err))
		}
	}
	key := enc(op.K)
	switch op.F {
	case family.RegisterWrite:
		r, err := c.post(ctx, base, "/v3/kv/put", map[string]any{"key": key, "value": enc(op.V.(string))})
		if err != nil {
			return nil, err
		}
		return uint64(r.Header.Revision), nil
	case family.RegisterCAS:
		pair := op.V.([]string)
		r, err := c.post(ctx, base, "/v3/kv/txn", map[string]any{
			"compare": []any{map[string]any{"key": key, "result": "EQUAL", "target": "VALUE", "value": enc(pair[0])}},
			"success": []any{map[string]any{"request_put": map[string]any{"key": key, "value": enc(pair[1])}}},
		})
		if err != nil {
			return nil, err
		}
		if !r.Succeeded {
			return nil, runner.Definite(errors.New("cas: current value differs"))
		}
		return uint64(r.Header.Revision), nil
	case family.RegisterReadF:
		body := map[string]any{"key": key}
		if op.Node != "" {
			pinned, err := c.fence(ctx, op.Node)
			if err != nil {
				return nil, runner.Definite(err)
			}
			base = pinned
			body["serializable"] = true
		}
		r, err := c.post(ctx, base, "/v3/kv/range", body)
		if err != nil {
			return nil, runner.Definite(err)
		}
		switch len(r.Kvs) {
		case 0:
			return nil, nil
		case 1:
			value, err := base64.StdEncoding.DecodeString(r.Kvs[0].Value)
			if err != nil {
				return nil, runner.Definite(err)
			}
			return string(value), nil
		}
		return nil, runner.Definite(fmt.Errorf("range: %d values for one key", len(r.Kvs)))
	case family.RegisterWatch:
		return c.watch(ctx, base, op.V.(family.WatchQuery).From)
	case family.RegisterHistory:
		at := slices.Index(c.names, op.Node)
		if at < 0 {
			return nil, runner.Definite(fmt.Errorf("unknown node %q", op.Node))
		}
		events, err := c.history(ctx, c.members[at])
		if err != nil {
			return nil, runner.Definite(err)
		}
		return events, nil
	case family.RegisterReplicas:
		copies, err := c.replicas(ctx, op.Node, op.V.(family.ReplicaQuery).Keys)
		if err != nil {
			return nil, runner.Definite(err)
		}
		return copies, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

// A member answers a put only after it has applied the put, and it applies
// entries in log order. Once the fence put returns, a serializable range on
// that member sees every write acknowledged before the fence started.
func (c client) fence(ctx context.Context, node string) (string, error) {
	at := slices.Index(c.names, node)
	if at < 0 {
		return "", fmt.Errorf("unknown node %q", node)
	}
	base := c.members[at]
	value := fmt.Sprintf("%016x", sdk.Random())
	if _, err := c.post(ctx, base, "/v3/kv/put", map[string]any{"key": enc("fence-" + node), "value": enc(value)}); err != nil {
		return "", fmt.Errorf("fence on %s: %w", node, err)
	}
	return base, nil
}

func (c client) replicas(ctx context.Context, node string, keys []string) (map[string]family.Copy, error) {
	base, err := c.fence(ctx, node)
	if err != nil {
		return nil, err
	}
	sorted := slices.Sorted(slices.Values(keys))
	r, err := c.post(ctx, base, "/v3/kv/range", map[string]any{
		"key":          enc(sorted[0]),
		"range_end":    enc(sorted[len(sorted)-1] + "\x00"),
		"serializable": true,
	})
	if err != nil {
		return nil, err
	}
	copies := make(map[string]family.Copy, len(keys))
	for _, key := range keys {
		copies[key] = family.Copy{}
	}
	for _, entry := range r.Kvs {
		name, err := base64.StdEncoding.DecodeString(entry.Key)
		if err != nil {
			return nil, err
		}
		if _, wanted := copies[string(name)]; !wanted {
			continue
		}
		value, err := base64.StdEncoding.DecodeString(entry.Value)
		if err != nil {
			return nil, err
		}
		text := string(value)
		copies[string(name)] = family.Copy{Value: &text, Meta: map[string]uint64{
			"create_revision": uint64(entry.CreateRevision),
			"mod_revision":    uint64(entry.ModRevision),
			"version":         uint64(entry.Version),
		}}
	}
	return copies, nil
}

// Only a refused or failed dial is definite. The gateway answers every other
// error after the request may have reached the raft log.
func (c client) defragmentMember(ctx context.Context, base string) {
	now := time.Now()
	deadline := now.Add(defragmentLimit)
	if parent, ok := ctx.Deadline(); ok && parent.Sub(now)/2 < defragmentLimit {
		deadline = now.Add(parent.Sub(now) / 2)
	}
	maintenance, cancel := context.WithDeadline(ctx, deadline)
	defer cancel()
	_, _ = c.post(maintenance, base, "/v3/maintenance/defragment", map[string]any{})
}

// The context's own deadline timer may not have fired yet when the deadline
// has already passed, so the clock is checked as well as the context.
func expired(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if deadline, ok := ctx.Deadline(); ok && !time.Now().Before(deadline) {
		return context.DeadlineExceeded
	}
	return nil
}

func (c client) post(ctx context.Context, base, path string, body any) (reply, error) {
	data, err := json.Marshal(body)
	if err != nil {
		return reply{}, runner.Definite(err)
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, base+path, bytes.NewReader(data))
	if err != nil {
		return reply{}, runner.Definite(err)
	}
	req.Header.Set("Content-Type", "application/json")
	checked := linearizable(path, body)
	floor, from := acknowledged.floor()
	resp, err := c.http.Do(req)
	if err != nil {
		var opErr *net.OpError
		if errors.As(err, &opErr) && opErr.Op == "dial" {
			return reply{}, runner.Definite(err)
		}
		return reply{}, err
	}
	defer resp.Body.Close()
	text, err := io.ReadAll(resp.Body)
	if err != nil {
		return reply{}, err
	}
	var r reply
	if err := json.Unmarshal(text, &r); err != nil {
		return reply{}, fmt.Errorf("%s: status %d: %q", path, resp.StatusCode, text)
	}
	if resp.StatusCode != http.StatusOK || r.Error != "" || r.Message != "" {
		return reply{}, fmt.Errorf("%s: status %d: %s", path, resp.StatusCode, strings.TrimSpace(string(text)))
	}
	if checked {
		acknowledged.answered(base, path, floor, from, uint64(r.Header.Revision))
	}
	return r, nil
}

func (a adapter) Ready(ctx context.Context) error {
	c := client{http: newHTTP(false)}
	return runner.AnyMember(ctx, a.members, func(ctx context.Context, base string) error {
		if _, err := c.post(ctx, base, "/v3/kv/put", map[string]any{"key": enc("ready"), "value": enc("1")}); err != nil {
			return fmt.Errorf("%s: %w", base, err)
		}
		return nil
	})
}

// A watch runs until shortly before the operation deadline, in up to three
// segments on drawn members. Each later segment resumes from the revision after
// the highest event delivered, as a client does when it fails over to another
// endpoint, and a segment that breaks hands over to the next. Events delivered
// before a stream breaks are kept, because the checks bound completeness by the
// last delivered revision. Each event carries its segment's start revision. A
// watch changes no key, so one that delivered nothing after a segment error
// fails definitely; one whose segments all stopped cleanly succeeds even when
// empty.
func (c client) watch(ctx context.Context, base string, from uint64) ([]family.Event, error) {
	return c.segmented(ctx, base, from, 1+int(sdk.Below("etcd.watch.segments", 3)))
}

func (c client) segmented(ctx context.Context, base string, from uint64, segments int) ([]family.Event, error) {
	deadline, ok := ctx.Deadline()
	if !ok {
		deadline = time.Now().Add(time.Second)
	}
	end := deadline.Add(-min(watchSpare, time.Until(deadline)/4))
	events := []family.Event{}
	next := from
	var failed error
	for segment := range segments {
		if segment > 0 {
			base = c.members[sdk.Below("etcd.server", uint64(len(c.members)))]
		}
		stop := time.Now().Add(time.Until(end) / time.Duration(segments-segment))
		got, err := c.stream(ctx, base, next, stop, 0)
		events = append(events, got...)
		for _, event := range got {
			next = max(next, event.Revision+1)
		}
		if err != nil {
			failed = err
		}
		if ctx.Err() != nil || !time.Now().Before(end) {
			break
		}
	}
	if len(events) == 0 && failed != nil {
		return nil, runner.Definite(failed)
	}
	return events, nil
}

// A member's history is a watch from revision 1 on that member, read until it
// delivers the highest revision of any register key the member holds.
func (c client) history(ctx context.Context, base string) ([]family.Event, error) {
	r, err := c.post(ctx, base, "/v3/kv/range", map[string]any{
		"key":          enc(watchFirst),
		"range_end":    enc(watchEnd),
		"serializable": true,
	})
	if err != nil {
		return nil, err
	}
	var through uint64
	for _, entry := range r.Kvs {
		through = max(through, uint64(entry.ModRevision))
	}
	if through == 0 {
		return []family.Event{}, nil
	}
	deadline, ok := ctx.Deadline()
	if !ok {
		deadline = time.Now().Add(time.Second)
	}
	events, err := c.stream(ctx, base, 1, deadline.Add(-min(watchSpare, time.Until(deadline)/4)), through)
	if err == nil && (len(events) == 0 || events[len(events)-1].Revision < through) {
		err = fmt.Errorf("history ended before revision %d", through)
	}
	if err != nil {
		return nil, err
	}
	return events, nil
}

type watchReply struct {
	Result struct {
		Canceled        bool    `json:"canceled"`
		CompactRevision number  `json:"compact_revision"`
		CancelReason    string  `json:"cancel_reason"`
		Events          []event `json:"events"`
	} `json:"result"`
	Error *struct {
		Message string `json:"message"`
	} `json:"error"`
}

type event struct {
	Type string `json:"type"`
	Kv   kv     `json:"kv"`
}

func (c client) stream(ctx context.Context, base string, from uint64, stop time.Time, through uint64) ([]family.Event, error) {
	node := c.names[slices.Index(c.members, base)]
	ctx, cancel := context.WithDeadline(ctx, stop)
	defer cancel()
	data, err := json.Marshal(map[string]any{"create_request": map[string]any{
		"key":            enc(watchFirst),
		"range_end":      enc(watchEnd),
		"start_revision": from,
	}})
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, base+"/v3/watch", bytes.NewReader(data))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	resp, err := c.http.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		text, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("/v3/watch: status %d: %s", resp.StatusCode, strings.TrimSpace(string(text)))
	}
	events := []family.Event{}
	decoder := json.NewDecoder(resp.Body)
	for {
		var message watchReply
		if err := decoder.Decode(&message); err != nil {
			if errors.Is(err, context.DeadlineExceeded) && errors.Is(ctx.Err(), context.DeadlineExceeded) && !time.Now().Before(stop) {
				return events, nil
			}
			return events, fmt.Errorf("/v3/watch: %w", err)
		}
		if message.Error != nil {
			return events, fmt.Errorf("/v3/watch: %s", message.Error.Message)
		}
		if message.Result.Canceled {
			return events, fmt.Errorf("/v3/watch canceled at compact revision %d: %s", message.Result.CompactRevision, message.Result.CancelReason)
		}
		frame := make([]family.Event, 0, len(message.Result.Events))
		for _, raw := range message.Result.Events {
			name, err := base64.StdEncoding.DecodeString(raw.Kv.Key)
			if err != nil {
				return events, err
			}
			if raw.Kv.ModRevision == 0 || len(name) == 0 {
				return events, errors.New("/v3/watch: event without a key or revision")
			}
			delivered := family.Event{Revision: uint64(raw.Kv.ModRevision), Key: string(name), Node: node, Start: from}
			if raw.Type != "DELETE" {
				value, err := base64.StdEncoding.DecodeString(raw.Kv.Value)
				if err != nil {
					return events, err
				}
				text := string(value)
				delivered.Value = &text
			}
			frame = append(frame, delivered)
		}
		events = append(events, frame...)
		if through > 0 && len(events) > 0 && events[len(events)-1].Revision >= through {
			return events, nil
		}
	}
}

func main() {
	count := 3
	if text := os.Getenv(membersEnv); text != "" {
		n, err := strconv.Atoi(text)
		if err != nil || n < 1 {
			fmt.Fprintf(os.Stderr, "bad %s %q\n", membersEnv, text)
			os.Exit(2)
		}
		count = n
	}
	members := []string{"http://127.0.0.1:2379"}
	if count > 1 {
		members = make([]string, count)
		for i := range members {
			members[i] = fmt.Sprintf("http://10.77.0.%d:2379", i+1)
		}
	}
	names := make([]string, count)
	for i := range names {
		names[i] = fmt.Sprintf("m%d", i+1)
	}
	var watch uint64
	if text := os.Getenv(watchEnv); text != "" {
		n, err := strconv.ParseUint(text, 10, 64)
		if err != nil {
			fmt.Fprintf(os.Stderr, "bad %s %q\n", watchEnv, text)
			os.Exit(2)
		}
		watch = n
	}
	var defragment uint64
	if text := os.Getenv(defragmentEnv); text != "" && len(os.Args) > 1 && os.Args[1] == "drive" {
		n, err := strconv.ParseUint(text, 10, 64)
		if err != nil || n > sdk.MaxChoiceOptions {
			fmt.Fprintf(os.Stderr, "bad %s %q: want 0 to %d\n", defragmentEnv, text, sdk.MaxChoiceOptions)
			os.Exit(2)
		}
		defragment = n
	}
	sdk.Declare(sdk.KindAlways, revisionNeverFalls)
	runner.Main(adapter{
		members:    members,
		names:      names,
		watch:      watch,
		defragment: defragment,
		writesOnly: flagEnv(writesEnv),
		keepAlive:  flagEnv(keepAliveEnv),
	})
}

func flagEnv(name string) bool {
	switch text := os.Getenv(name); text {
	case "", "0":
		return false
	case "1":
		return true
	default:
		fmt.Fprintf(os.Stderr, "bad %s %q\n", name, text)
		os.Exit(2)
	}
	return false
}
