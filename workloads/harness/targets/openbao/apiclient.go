// SPDX-License-Identifier: AGPL-3.0-or-later

package main

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"sync"

	"github.com/openbao/openbao/api/v2"

	"harmony.local/harness/kit/family"
	"harmony.local/harness/kit/runner"
)

// apiClient drives the same operations through the upstream Go client with
// its default retries and redirect handling, so the history shows what an
// application built on that client observes.
type apiClient struct {
	nodes   []string
	kv      map[string]*api.KVv2
	mu      sync.Mutex
	version map[string]int
}

func newAPIClient(nodes []string, token string) (*apiClient, error) {
	c := &apiClient{nodes: nodes, kv: map[string]*api.KVv2{}, version: map[string]int{}}
	for _, node := range nodes {
		config := api.DefaultConfig()
		config.Address = "http://" + node
		client, err := api.NewClient(config)
		if err != nil {
			return nil, err
		}
		client.SetToken(token)
		c.kv[node] = client.KVv2("secret")
	}
	return c, nil
}

func (c *apiClient) Close() {}

func (c *apiClient) remember(key, value string, version int) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.version[key+"\x00"+value] = version
}

func (c *apiClient) known(key, value string) (int, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	version, ok := c.version[key+"\x00"+value]
	return version, ok
}

func (c *apiClient) Invoke(ctx context.Context, op family.Op) (any, error) {
	kv := c.kv[pick(c.nodes, op)]
	switch op.F {
	case family.RegisterWrite:
		value := op.V.(string)
		secret, err := kv.Put(ctx, op.K, map[string]any{"v": value})
		if err != nil {
			return nil, err
		}
		c.remember(op.K, value, secret.VersionMetadata.Version)
		return nil, nil
	case family.RegisterCAS:
		pair := op.V.([]string)
		expected, ok := c.known(op.K, pair[0])
		if !ok {
			current, version, err := c.get(ctx, kv, op.K)
			if err != nil {
				return nil, runner.Definite(err)
			}
			if current == nil || *current != pair[0] {
				return nil, runner.Definite(errors.New("cas: current value differs"))
			}
			expected = version
		}
		secret, err := kv.Put(ctx, op.K, map[string]any{"v": pair[1]}, api.WithCheckAndSet(expected))
		if err != nil {
			return nil, classifyAPI(err)
		}
		c.remember(op.K, pair[1], secret.VersionMetadata.Version)
		return nil, nil
	case family.RegisterReadF:
		current, _, err := c.get(ctx, kv, op.K)
		if err != nil {
			return nil, runner.Definite(err)
		}
		if current == nil {
			return nil, nil
		}
		return *current, nil
	}
	return nil, runner.Definite(fmt.Errorf("unknown operation %q", op.F))
}

// The client reports a check-and-set conflict as an answer from the server,
// which an application reads as a write that did not happen.
func classifyAPI(err error) error {
	var response *api.ResponseError
	if errors.As(err, &response) && response.StatusCode == http.StatusBadRequest &&
		strings.Contains(err.Error(), "check-and-set parameter did not match") {
		return runner.Definite(err)
	}
	return err
}

func (c *apiClient) get(ctx context.Context, kv *api.KVv2, key string) (*string, int, error) {
	secret, err := kv.Get(ctx, key)
	if errors.Is(err, api.ErrSecretNotFound) {
		return nil, 0, nil
	}
	if err != nil {
		return nil, 0, err
	}
	value, ok := secret.Data["v"].(string)
	if !ok {
		return nil, 0, fmt.Errorf("get %s: %w", key, errNotFound)
	}
	c.remember(key, value, secret.VersionMetadata.Version)
	return &value, secret.VersionMetadata.Version, nil
}
