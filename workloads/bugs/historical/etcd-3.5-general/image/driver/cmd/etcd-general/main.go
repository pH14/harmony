// SPDX-License-Identifier: AGPL-3.0-or-later

// etcd-general runs the general-discovery etcd workload: four clients, each
// owning keys gen/<w>/<n> and drawing operations from the searchable choice
// stream. See ../../README.md for the frozen specification.
package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strconv"
	"sync"
	"time"

	clientv3 "go.etcd.io/etcd/client/v3"

	"harmony-etcd-general/choice"
	"harmony-etcd-general/history"
)

const (
	clients        = 4
	requestTimeout = 5 * time.Second
	journalDir     = "/tmp/etcd/journal"
	linearizableID = "linearizable reads observe acknowledged writes"
)

var endpoints = []string{
	"http://127.0.0.1:2379",
	"http://127.0.0.1:2381",
	"http://127.0.0.1:2383",
}

type client struct {
	id      int
	cli     *clientv3.Client
	stream  *choice.Stream
	journal *os.File
	path    string
	nextOp  uint64
	nextKey int
	keys    []string
	history *history.Journal
	single  map[string]*clientv3.Client

	linearizableReported bool
}

func main() {
	choice.DeclareAlways(linearizableID)
	if err := os.MkdirAll(journalDir, 0o755); err != nil {
		log.Fatal(err)
	}
	var group sync.WaitGroup
	for w := 1; w <= clients; w++ {
		c, err := open(w)
		if err != nil {
			log.Fatal(err)
		}
		group.Add(1)
		go func() {
			defer group.Done()
			c.run()
		}()
	}
	group.Wait()
}

func open(w int) (*client, error) {
	path := filepath.Join(journalDir, fmt.Sprintf("general-%d", w))
	c := &client{id: w, path: path, stream: choice.New(), nextOp: 1, history: history.NewJournal(),
		single: map[string]*clientv3.Client{}}
	if existing, err := os.Open(path); err == nil {
		_ = c.history.Read(existing)
		existing.Close()
		for _, name := range c.history.Keys() {
			c.keys = append(c.keys, name)
			for _, op := range c.history.OnKey(name) {
				if op.ID >= c.nextOp {
					c.nextOp = op.ID + 1
				}
			}
		}
		c.nextKey = len(c.keys)
	}
	journal, err := os.OpenFile(path, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0o644)
	if err != nil {
		return nil, err
	}
	c.journal = journal
	cli, err := clientv3.New(clientv3.Config{Endpoints: endpoints, DialTimeout: requestTimeout})
	if err != nil {
		return nil, err
	}
	c.cli = cli
	return c, nil
}

func (c *client) write(line string) {
	if _, err := c.journal.WriteString(line); err != nil {
		log.Fatal(err)
	}
	c.history.Record(line)
}

func (c *client) ctx() (context.Context, context.CancelFunc) {
	return context.WithTimeout(context.Background(), requestTimeout)
}

func (c *client) target() string {
	if len(c.keys) == 0 || c.stream.Below(2) == 0 {
		name := fmt.Sprintf("gen/%d/%d", c.id, c.nextKey)
		c.nextKey++
		c.keys = append(c.keys, name)
		return name
	}
	return c.keys[c.stream.Below(uint64(len(c.keys)))]
}

func (c *client) value() string {
	return fmt.Sprintf("v-%d-%d-%d", c.id, c.nextOp, c.stream.Next()%1000000)
}

func (c *client) run() {
	for {
		c.stream.Sync()
		switch c.stream.Below(8) {
		case 0:
			c.put()
		case 1:
			c.del()
		case 2:
			c.cas()
		case 3:
			c.get()
		case 4:
			c.getSerializable()
		case 5:
			c.rangeOwned()
		case 6:
			c.lease()
		default:
			c.compact()
		}
		c.stream.Think()
	}
}

func (c *client) intent(kind history.Kind, name, value string) uint64 {
	id := c.nextOp
	c.nextOp++
	c.write(history.IntentLine(id, kind, name, value))
	return id
}

func (c *client) put() {
	name, value := c.target(), c.value()
	id := c.intent(history.Put, name, value)
	ctx, cancel := c.ctx()
	response, err := c.cli.Put(ctx, name, value)
	cancel()
	if err == nil && response.Header != nil {
		c.write(history.AckLine(id, response.Header.Revision, true))
	}
}

func (c *client) del() {
	if len(c.keys) == 0 {
		return
	}
	name := c.keys[c.stream.Below(uint64(len(c.keys)))]
	id := c.intent(history.Delete, name, "-")
	ctx, cancel := c.ctx()
	response, err := c.cli.Delete(ctx, name)
	cancel()
	if err == nil && response.Header != nil {
		c.write(history.AckLine(id, response.Header.Revision, response.Deleted > 0))
	}
}

func (c *client) ownHistory(name string) []history.Op { return c.history.OnKey(name) }

func (c *client) cas() {
	if len(c.keys) == 0 {
		return
	}
	name := c.keys[c.stream.Below(uint64(len(c.keys)))]
	latest, _ := history.Latest(c.ownHistory(name))
	expected := int64(0)
	if latest != nil && latest.Writes() {
		expected = latest.Revision
	}
	value := c.value()
	id := c.intent(history.CAS, name, value)
	ctx, cancel := c.ctx()
	response, err := c.cli.Txn(ctx).
		If(clientv3.Compare(clientv3.ModRevision(name), "=", expected)).
		Then(clientv3.OpPut(name, value)).
		Else(clientv3.OpGet(name)).
		Commit()
	cancel()
	if err == nil && response.Header != nil {
		c.write(history.AckLine(id, response.Header.Revision, response.Succeeded))
	}
}

func (c *client) check(name string, present bool, data string, modRevision int64) {
	latest, uncertain := history.Latest(c.ownHistory(name))
	if uncertain {
		return
	}
	var ok bool
	var detail string
	switch {
	case latest == nil || !latest.Writes():
		ok = !present
		detail = fmt.Sprintf("%s present with %q, history says absent", name, data)
	default:
		ok = present && data == latest.Value && modRevision == latest.Revision
		detail = fmt.Sprintf("%s read %q at %d, history says %q at %d", name, data, modRevision, latest.Value, latest.Revision)
	}
	if ok && c.linearizableReported {
		return
	}
	c.linearizableReported = c.linearizableReported || ok
	choice.Always(linearizableID, ok, detail)
}

func (c *client) get() {
	if len(c.keys) == 0 {
		return
	}
	name := c.keys[c.stream.Below(uint64(len(c.keys)))]
	ctx, cancel := c.ctx()
	response, err := c.cli.Get(ctx, name)
	cancel()
	if err != nil {
		return
	}
	if len(response.Kvs) == 0 {
		c.check(name, false, "", 0)
		return
	}
	kv := response.Kvs[0]
	c.check(name, true, string(kv.Value), kv.ModRevision)
}

func (c *client) getSerializable() {
	if len(c.keys) == 0 {
		return
	}
	name := c.keys[c.stream.Below(uint64(len(c.keys)))]
	endpoint := endpoints[c.stream.Below(uint64(len(endpoints)))]
	single := c.single[endpoint]
	if single == nil {
		var err error
		single, err = clientv3.New(clientv3.Config{Endpoints: []string{endpoint}, DialTimeout: requestTimeout})
		if err != nil {
			return
		}
		c.single[endpoint] = single
	}
	ctx, cancel := c.ctx()
	_, _ = single.Get(ctx, name, clientv3.WithSerializable())
	cancel()
}

func (c *client) rangeOwned() {
	prefix := fmt.Sprintf("gen/%d/", c.id)
	ctx, cancel := c.ctx()
	response, err := c.cli.Get(ctx, prefix, clientv3.WithPrefix())
	cancel()
	if err != nil {
		return
	}
	seen := map[string]bool{}
	for _, kv := range response.Kvs {
		seen[string(kv.Key)] = true
		c.check(string(kv.Key), true, string(kv.Value), kv.ModRevision)
	}
	for _, name := range c.keys {
		if !seen[name] {
			c.check(name, false, "", 0)
		}
	}
}

func (c *client) lease() {
	ttls := []int64{1, 2, 5, 10}
	ctx, cancel := c.ctx()
	defer cancel()
	grant, err := c.cli.Grant(ctx, ttls[c.stream.Below(uint64(len(ttls)))])
	if err != nil {
		return
	}
	name := "lease/" + strconv.Itoa(c.id) + "/" + strconv.FormatUint(c.stream.Below(64), 10)
	if _, err := c.cli.Put(ctx, name, c.value(), clientv3.WithLease(grant.ID)); err != nil {
		return
	}
	if c.stream.Below(2) == 0 {
		_, _ = c.cli.Revoke(ctx, grant.ID)
	}
}

func (c *client) compact() {
	ctx, cancel := c.ctx()
	defer cancel()
	status, err := c.cli.Get(ctx, "gen/", clientv3.WithPrefix(), clientv3.WithCountOnly())
	if err != nil || status.Header == nil {
		return
	}
	revision := status.Header.Revision - int64(c.stream.Below(101))
	if revision < 1 {
		return
	}
	_, _ = c.cli.Compact(ctx, revision)
}
