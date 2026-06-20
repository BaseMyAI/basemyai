package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"
)

type rememberRequest struct {
	AgentID string `json:"agent_id"`
	Text    string `json:"text"`
	Layer   string `json:"layer"`
}

type idResponse struct {
	ID string `json:"id"`
}

type recallRequest struct {
	AgentID string `json:"agent_id"`
	Query   string `json:"query"`
	K       int    `json:"k"`
}

type record struct {
	ID    string  `json:"id"`
	Text  string  `json:"text"`
	Layer string  `json:"layer"`
	Score float64 `json:"score"`
}

type recallResponse struct {
	Results   []record `json:"results"`
	Truncated bool     `json:"truncated"`
}

func main() {
	baseURL := env("BASEMYAI_REST_URL", "http://127.0.0.1:7743/v1")
	token := requiredEnv("BASEMYAI_REST_API_KEY")
	agentID := env("BASEMYAI_AGENT_ID", "go-example")

	client := &http.Client{Timeout: 10 * time.Second}

	var created idResponse
	postJSON(client, baseURL+"/remember", token, rememberRequest{
		AgentID: agentID,
		Text:    "BaseMyAI stores encrypted local agent memory.",
		Layer:   "semantic",
	}, &created)
	fmt.Println("remembered:", created.ID)

	var recalled recallResponse
	postJSON(client, baseURL+"/recall", token, recallRequest{
		AgentID: agentID,
		Query:   "encrypted local memory",
		K:       5,
	}, &recalled)
	for _, hit := range recalled.Results {
		fmt.Printf("%.3f [%s] %s\n", hit.Score, hit.Layer, hit.Text)
	}
}

func postJSON(client *http.Client, url string, token string, in any, out any) {
	body, err := json.Marshal(in)
	check(err)

	req, err := http.NewRequest(http.MethodPost, url, bytes.NewReader(body))
	check(err)
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json")

	resp, err := client.Do(req)
	check(err)
	defer resp.Body.Close()

	raw, err := io.ReadAll(resp.Body)
	check(err)
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		panic(fmt.Sprintf("%s: %s", resp.Status, strings.TrimSpace(string(raw))))
	}
	check(json.Unmarshal(raw, out))
}

func env(name string, fallback string) string {
	value := os.Getenv(name)
	if value == "" {
		return fallback
	}
	return value
}

func requiredEnv(name string) string {
	value := os.Getenv(name)
	if value == "" {
		panic(name + " is required")
	}
	return value
}

func check(err error) {
	if err != nil {
		panic(err)
	}
}
