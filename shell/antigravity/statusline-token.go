package main

import (
	"bytes"
	"crypto/rand"
	"encoding/json"
	"fmt"
	"io"
	"math/big"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

type Model struct {
	DisplayName string `json:"display_name"`
	ID          string `json:"id"`
}

type CurrentUsage struct {
	InputTokens               int64 `json:"input_tokens"`
	CacheReadInputTokens      int64 `json:"cache_read_input_tokens"`
	CacheCreationInputTokens int64 `json:"cache_creation_input_tokens"`
}

type ContextWindow struct {
	TotalInputTokens             int64        `json:"total_input_tokens"`
	TotalOutputTokens            int64        `json:"total_output_tokens"`
	TotalCacheReadTokens         int64        `json:"total_cache_read_tokens"`
	TotalCacheWriteTokens        int64        `json:"total_cache_write_tokens"`
	TotalReasoningTokens         int64        `json:"total_reasoning_tokens"`
	TotalTokens                  interface{}  `json:"total_tokens"`
	LastCallInputTokens          int64        `json:"last_call_input_tokens"`
	LastCallOutputTokens         int64        `json:"last_call_output_tokens"`
	ContextWindowSize            int64        `json:"context_window_size"`
	CurrentUsage                 CurrentUsage `json:"current_usage"`
	CurrentContextUsedPercentage interface{}  `json:"current_context_used_percentage"`
	UsedPercentage               interface{}  `json:"used_percentage"`
}

type Cost struct {
	TotalApiDurationMs   float64     `json:"total_api_duration_ms"`
	TotalDurationMs      float64     `json:"total_duration_ms"`
	TotalPremiumRequests float64     `json:"total_premium_requests"`
	TotalLinesAdded      interface{} `json:"total_lines_added"`
	TotalLinesRemoved    interface{} `json:"total_lines_removed"`
}

type FilesMetrics struct {
	TotalLinesAdded   int64 `json:"total_lines_added"`
	TotalLinesRemoved int64 `json:"total_lines_removed"`
}

type Metrics struct {
	Files FilesMetrics `json:"files"`
}

type Workspace struct {
	CurrentDir string `json:"current_dir"`
}

type InputData struct {
	ConversationID string        `json:"conversation_id"`
	SessionID      string        `json:"session_id"`
	SessionName    string        `json:"session_name"`
	TranscriptPath string        `json:"transcript_path"`
	Cwd            string        `json:"cwd"`
	Workspace      Workspace     `json:"workspace"`
	Version        string        `json:"version"`
	Model          Model         `json:"model"`
	ModelName      string        `json:"modelName"`
	CurrentModel   string        `json:"current_model"`
	ContextWindow  ContextWindow `json:"context_window"`
	Cost           Cost          `json:"cost"`
	Metrics        Metrics       `json:"metrics"`
}

type StateData struct {
	SessionID         string `json:"session_id"`
	SessionName       string `json:"session_name"`
	TranscriptPath    string `json:"transcript_path"`
	Model             string `json:"model"`
	ModelID           string `json:"model_id"`
	TurnNo            int64  `json:"turn_no"`
	InputTokens       int64  `json:"input_tokens"`
	OutputTokens      int64  `json:"output_tokens"`
	CacheReadTokens   int64  `json:"cache_read_tokens"`
	CacheWriteTokens  int64  `json:"cache_write_tokens"`
	ReasoningTokens   int64  `json:"reasoning_tokens"`
	TotalTokens       int64  `json:"total_tokens"`
}

type TokenDetail struct {
	Input         int64 `json:"input"`
	Output        int64 `json:"output"`
	CacheRead     int64 `json:"cache_read"`
	CacheWrite    int64 `json:"cache_write"`
	Reasoning     int64 `json:"reasoning"`
	Total         int64 `json:"total"`
	LastCallInput int64 `json:"last_call_input"`
	LastCallOutput int64 `json:"last_call_output"`
}

type TokenDelta struct {
	Input      int64 `json:"input"`
	Output     int64 `json:"output"`
	CacheRead  int64 `json:"cache_read"`
	CacheWrite int64 `json:"cache_write"`
	Reasoning  int64 `json:"reasoning"`
	Total      int64 `json:"total"`
}

type ContextDetail struct {
	CurrentContextTokens         int64  `json:"current_context_tokens"`
	DisplayedContextLimit        int64  `json:"displayed_context_limit"`
	CurrentContextUsedPercentage string `json:"current_context_used_percentage"`
}

type CostDetail struct {
	TotalApiDurationMs  float64 `json:"total_api_duration_ms"`
	TotalDurationMs     float64 `json:"total_duration_ms"`
	TotalPremiumRequests float64 `json:"total_premium_requests"`
	TotalLinesAdded     int64   `json:"total_lines_added"`
	TotalLinesRemoved   int64   `json:"total_lines_removed"`
}

type JSONLEntry struct {
	Timestamp      string        `json:"timestamp"`
	SessionID      string        `json:"session_id"`
	SessionName    string        `json:"session_name"`
	TranscriptPath string        `json:"transcript_path"`
	Cwd            string        `json:"cwd"`
	Version        string        `json:"version"`
	TurnNo         int64         `json:"turn_no"`
	Model          string        `json:"model"`
	ModelID        string        `json:"model_id"`
	PreviousModel  string        `json:"previous_model,omitempty"`
	ModelChanged   bool          `json:"model_changed"`
	Tokens         TokenDetail   `json:"tokens"`
	DeltaTokens    TokenDelta    `json:"delta_tokens"`
	Context        ContextDetail `json:"context"`
	Cost           CostDetail    `json:"cost"`
}

func generateUUID() string {
	b := make([]byte, 16)
	_, err := io.ReadFull(rand.Reader, b)
	if err != nil {
		n, _ := rand.Int(rand.Reader, big.NewInt(100000))
		m, _ := rand.Int(rand.Reader, big.NewInt(100000))
		return fmt.Sprintf("%d-%d", n.Int64(), m.Int64())
	}
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:])
}

func formatPercentage(p interface{}) string {
	if p == nil {
		return ""
	}
	var val float64
	var isNum bool

	switch v := p.(type) {
	case float64:
		val = v
		isNum = true
	case int64:
		val = float64(v)
		isNum = true
	case int:
		val = float64(v)
		isNum = true
	case string:
		clean := strings.TrimSuffix(v, "%")
		clean = strings.TrimSpace(clean)
		if f, err := strconv.ParseFloat(clean, 64); err == nil {
			val = f
			isNum = true
		} else {
			return v
		}
	}

	if isNum {
		s := fmt.Sprintf("%.2f", val)
		s = strings.TrimSuffix(s, ".00")
		if strings.Contains(s, ".") {
			s = strings.TrimSuffix(s, "0")
		}
		return s
	}
	return fmt.Sprintf("%v", p)
}

func getInt64FromInterface(val interface{}) int64 {
	if val == nil {
		return 0
	}
	switch v := val.(type) {
	case float64:
		return int64(v)
	case int64:
		return v
	case int:
		return int64(v)
	case string:
		if i, err := strconv.ParseInt(v, 10, 64); err == nil {
			return i
		}
	}
	return 0
}

func main() {
	// Read standard input
	inputBytes, err := io.ReadAll(os.Stdin)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Error reading stdin: %v\n", err)
		os.Exit(1)
	}

	var input InputData
	if err := json.Unmarshal(inputBytes, &input); err != nil {
		input = InputData{}
	}

	homeDir, err := os.UserHomeDir()
	if err != nil {
		homeDir = "."
	}

	agDir := filepath.Join(homeDir, ".gemini", "antigravity-cli")
	usageDir := filepath.Join(agDir, "usage")
	_ = os.MkdirAll(usageDir, 0755)

	stateFile := filepath.Join(agDir, "statusline-state.json")
	debugLog := filepath.Join(agDir, "statusline-debug.log")
	now := time.Now()
	dateStr := now.Format("2006-01-02")
	jsonlFile := filepath.Join(usageDir, fmt.Sprintf("usage-%s.jsonl", dateStr))

	// Resolve session properties
	sessionID := input.ConversationID
	if sessionID == "" {
		sessionID = input.SessionID
	}
	if sessionID == "" {
		sessionID = fmt.Sprintf("%s-%s", now.Format("20060102-150405"), generateUUID()[:8])
	}

	sessionName := input.SessionName
	if sessionName == "" {
		if len(sessionID) >= 8 {
			sessionName = sessionID[:8]
		} else {
			sessionName = sessionID
		}
	}

	transcriptPath := input.TranscriptPath
	if transcriptPath != "" {
		transcriptPath = strings.ReplaceAll(transcriptPath, "/.gemini/antigravity/", "/.gemini/antigravity-cli/")
	}

	cwd := input.Cwd
	if cwd == "" {
		cwd = input.Workspace.CurrentDir
	}

	// Resolve model details
	model := input.Model.DisplayName
	if model == "" {
		model = input.Model.ID
	}
	if model == "" {
		model = input.ModelName
	}
	if model == "" {
		model = input.CurrentModel
	}
	if model == "" {
		model = "unknown"
	}

	modelID := input.Model.ID
	if modelID == "" {
		modelID = input.ModelName
	}
	if modelID == "" {
		modelID = input.CurrentModel
	}
	if modelID == "" {
		modelID = "unknown"
	}

	// Resolve tokens
	inputTokens := input.ContextWindow.TotalInputTokens
	outputTokens := input.ContextWindow.TotalOutputTokens

	cacheReadTokens := input.ContextWindow.TotalCacheReadTokens
	if cacheReadTokens == 0 {
		cacheReadTokens = input.ContextWindow.CurrentUsage.CacheReadInputTokens
	}

	cacheWriteTokens := input.ContextWindow.TotalCacheWriteTokens
	if cacheWriteTokens == 0 {
		cacheWriteTokens = input.ContextWindow.CurrentUsage.CacheCreationInputTokens
	}

	reasoningTokens := input.ContextWindow.TotalReasoningTokens

	var totalTokens int64
	if input.ContextWindow.TotalTokens != nil {
		totalTokens = getInt64FromInterface(input.ContextWindow.TotalTokens)
	}
	if totalTokens == 0 {
		totalTokens = inputTokens + outputTokens + cacheReadTokens + cacheWriteTokens + reasoningTokens
	}

	lastCallInputTokens := input.ContextWindow.LastCallInputTokens
	lastCallOutputTokens := input.ContextWindow.LastCallOutputTokens
	currentContextTokens := input.ContextWindow.CurrentUsage.InputTokens
	displayedContextLimit := input.ContextWindow.ContextWindowSize

	var currentContextUsedPercentage string
	if input.ContextWindow.CurrentContextUsedPercentage != nil {
		currentContextUsedPercentage = formatPercentage(input.ContextWindow.CurrentContextUsedPercentage)
	} else if input.ContextWindow.UsedPercentage != nil {
		currentContextUsedPercentage = formatPercentage(input.ContextWindow.UsedPercentage)
	}

	// Resolve cost details
	totalApiDurationMs := input.Cost.TotalApiDurationMs
	totalDurationMs := input.Cost.TotalDurationMs
	totalPremiumRequests := input.Cost.TotalPremiumRequests

	var totalLinesAdded int64
	if input.Cost.TotalLinesAdded != nil {
		totalLinesAdded = getInt64FromInterface(input.Cost.TotalLinesAdded)
	}
	if totalLinesAdded == 0 {
		totalLinesAdded = input.Metrics.Files.TotalLinesAdded
	}

	var totalLinesRemoved int64
	if input.Cost.TotalLinesRemoved != nil {
		totalLinesRemoved = getInt64FromInterface(input.Cost.TotalLinesRemoved)
	}
	if totalLinesRemoved == 0 {
		totalLinesRemoved = input.Metrics.Files.TotalLinesRemoved
	}

	// Read state data
	var previousSessionID string
	var previousModel string
	var previousTurnNo int64
	var previousInputTokens int64
	var previousOutputTokens int64
	var previousCacheReadTokens int64
	var previousCacheWriteTokens int64
	var previousReasoningTokens int64
	var previousTotalTokens int64

	if stateBytes, err := os.ReadFile(stateFile); err == nil {
		var state StateData
		if err := json.Unmarshal(stateBytes, &state); err == nil {
			previousSessionID = state.SessionID
			previousModel = state.Model
			previousTurnNo = state.TurnNo
			previousInputTokens = state.InputTokens
			previousOutputTokens = state.OutputTokens
			previousCacheReadTokens = state.CacheReadTokens
			previousCacheWriteTokens = state.CacheWriteTokens
			previousReasoningTokens = state.ReasoningTokens
			previousTotalTokens = state.TotalTokens
		}
	}

	// Reset state if session changes
	if previousSessionID != sessionID {
		previousModel = ""
		previousTurnNo = 0
		previousInputTokens = 0
		previousOutputTokens = 0
		previousCacheReadTokens = 0
		previousCacheWriteTokens = 0
		previousReasoningTokens = 0
		previousTotalTokens = 0
	}

	// Calculate deltas
	deltaInput := inputTokens - previousInputTokens
	if deltaInput < 0 {
		deltaInput = 0
	}
	deltaOutput := outputTokens - previousOutputTokens
	if deltaOutput < 0 {
		deltaOutput = 0
	}
	deltaCacheRead := cacheReadTokens - previousCacheReadTokens
	if deltaCacheRead < 0 {
		deltaCacheRead = 0
	}
	deltaCacheWrite := cacheWriteTokens - previousCacheWriteTokens
	if deltaCacheWrite < 0 {
		deltaCacheWrite = 0
	}
	deltaReasoning := reasoningTokens - previousReasoningTokens
	if deltaReasoning < 0 {
		deltaReasoning = 0
	}
	deltaTotal := totalTokens - previousTotalTokens
	if deltaTotal < 0 {
		deltaTotal = 0
	}

	if lastCallInputTokens == 0 && deltaInput > 0 {
		lastCallInputTokens = deltaInput
	}
	if lastCallOutputTokens == 0 && deltaOutput > 0 {
		lastCallOutputTokens = deltaOutput
	}

	modelChanged := false
	if previousModel != "" && previousModel != model {
		modelChanged = true
	}

	turnNo := previousTurnNo
	if deltaTotal > 0 {
		turnNo = previousTurnNo + 1

		// Write to JSONL
		entry := JSONLEntry{
			Timestamp:      now.Format(time.RFC3339),
			SessionID:      sessionID,
			SessionName:    sessionName,
			TranscriptPath: transcriptPath,
			Cwd:            cwd,
			Version:        input.Version,
			TurnNo:         turnNo,
			Model:          model,
			ModelID:        modelID,
			PreviousModel:  previousModel,
			ModelChanged:   modelChanged,
			Tokens: TokenDetail{
				Input:          inputTokens,
				Output:         outputTokens,
				CacheRead:      cacheReadTokens,
				CacheWrite:     cacheWriteTokens,
				Reasoning:      reasoningTokens,
				Total:          totalTokens,
				LastCallInput:  lastCallInputTokens,
				LastCallOutput: lastCallOutputTokens,
			},
			DeltaTokens: TokenDelta{
				Input:      deltaInput,
				Output:     deltaOutput,
				CacheRead:  deltaCacheRead,
				CacheWrite: deltaCacheWrite,
				Reasoning:  deltaReasoning,
				Total:      deltaTotal,
			},
			Context: ContextDetail{
				CurrentContextTokens:         currentContextTokens,
				DisplayedContextLimit:        displayedContextLimit,
				CurrentContextUsedPercentage: currentContextUsedPercentage,
			},
			Cost: CostDetail{
				TotalApiDurationMs:  totalApiDurationMs,
				TotalDurationMs:     totalDurationMs,
				TotalPremiumRequests: totalPremiumRequests,
				TotalLinesAdded:     totalLinesAdded,
				TotalLinesRemoved:   totalLinesRemoved,
			},
		}

		entryBytes, err := json.Marshal(entry)
		if err == nil {
			f, err := os.OpenFile(jsonlFile, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
			if err == nil {
				_, _ = f.Write(append(entryBytes, '\n'))
				_ = f.Close()
			}
		}
	}

	// Write State File
	newState := StateData{
		SessionID:        sessionID,
		SessionName:      sessionName,
		TranscriptPath:   transcriptPath,
		Model:            model,
		ModelID:          modelID,
		TurnNo:           turnNo,
		InputTokens:      inputTokens,
		OutputTokens:     outputTokens,
		CacheReadTokens:  cacheReadTokens,
		CacheWriteTokens: cacheWriteTokens,
		ReasoningTokens:  reasoningTokens,
		TotalTokens:      totalTokens,
	}

	if stateBytes, err := json.MarshalIndent(newState, "", "  "); err == nil {
		_ = os.WriteFile(stateFile, stateBytes, 0644)
	}

	// Delegate rendering to the original go statusline component (foreground statusline-go.exe)
	goStatusline := filepath.Join(agDir, "hooks", "statusline-go.exe")
	if info, err := os.Stat(goStatusline); err == nil && !info.IsDir() {
		cmd := exec.Command(goStatusline)
		cmd.Stdin = bytes.NewReader(inputBytes)
		
		var stdoutBuf, stderrBuf bytes.Buffer
		cmd.Stdout = &stdoutBuf
		cmd.Stderr = &stderrBuf

		if err := cmd.Run(); err == nil {
			os.Stdout.Write(stdoutBuf.Bytes())
			if stderrBuf.Len() > 0 {
				logDebug(debugLog, fmt.Sprintf("hooks statusline stderr: %s", stderrBuf.String()))
			}
		} else {
			logDebug(debugLog, fmt.Sprintf("Error running hooks statusline-go.exe: %v", err))
			os.Stdout.WriteString("statusline")
		}
	} else {
		os.Stdout.WriteString("statusline")
	}
}

func logDebug(filePath, msg string) {
	f, err := os.OpenFile(filePath, os.O_CREATE|os.O_WRONLY|os.O_APPEND, 0644)
	if err == nil {
		_, _ = f.WriteString(fmt.Sprintf("[%s] %s\n", time.Now().Format("2006-01-02T15:04:05"), msg))
		_ = f.Close()
	}
}
