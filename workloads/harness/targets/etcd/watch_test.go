// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func watchMember(t *testing.T, kvs string, frames ...string) client {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/v3/kv/range":
			fmt.Fprintf(w, `{"header":{"revision":"30"},"kvs":%s}`, kvs)
		case "/v3/watch":
			var body struct {
				Create struct {
					Start uint64 `json:"start_revision"`
				} `json:"create_request"`
			}
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				t.Errorf("watch body: %v", err)
			}
			flusher := w.(http.Flusher)
			for _, frame := range frames {
				fmt.Fprintln(w, frame)
				flusher.Flush()
			}
		default:
			t.Errorf("unexpected path %s", r.URL.Path)
		}
	}))
	t.Cleanup(server.Close)
	return client{members: []string{server.URL}, names: []string{"m1"}, http: newHTTP()}
}

func eventFrame(revision uint64, keys ...string) string {
	events := ""
	for index, key := range keys {
		if index > 0 {
			events += ","
		}
		events += fmt.Sprintf(`{"kv":{"key":%q,"value":%q,"mod_revision":"%d"}}`, enc(key), enc("v"), revision)
	}
	return fmt.Sprintf(`{"result":{"events":[%s]}}`, events)
}

func bounded(t *testing.T) context.Context {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	t.Cleanup(cancel)
	return ctx
}

func TestAnEventWithoutARevisionIsAnError(t *testing.T) {
	c := watchMember(t, `[]`, `{"result":{"events":[{}]}}`)
	events, err := c.stream(bounded(t), c.members[0], 10, time.Now().Add(time.Second), 0)
	if err == nil || len(events) != 0 {
		t.Fatalf("events %+v, error %v", events, err)
	}
}

func TestAHistoryReadKeepsEveryEventOfItsLastRevision(t *testing.T) {
	c := watchMember(t, `[{"key":"azA=","mod_revision":"20"},{"key":"azE=","mod_revision":"20"}]`,
		eventFrame(19, "k0"), eventFrame(20, "k0", "k1"))
	events, err := c.history(bounded(t), c.members[0])
	if err != nil {
		t.Fatal(err)
	}
	if len(events) != 3 || events[1].Key != "k0" || events[2].Key != "k1" || events[2].Revision != 20 {
		t.Fatalf("events %+v", events)
	}
}

func TestAWatchKeepsEventsFromABrokenStreamWithTheirStart(t *testing.T) {
	c := watchMember(t, `[]`, eventFrame(12, "k0"))
	got, err := c.watch(bounded(t), c.members[0], 12)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) == 0 || got[0].Revision != 12 || got[0].Start != 12 {
		t.Fatalf("events %+v", got)
	}
	for _, event := range got[1:] {
		if event.Start <= got[0].Revision {
			t.Fatalf("a resumed segment started at %d after revision %d", event.Start, got[0].Revision)
		}
	}
}

func TestAWatchThatDeliversNothingFails(t *testing.T) {
	c := watchMember(t, `[]`, `{"error":{"message":"etcdserver: mvcc: required revision has been compacted"}}`)
	got, err := c.watch(bounded(t), c.members[0], 12)
	if err == nil {
		t.Fatalf("events %+v", got)
	}
}
