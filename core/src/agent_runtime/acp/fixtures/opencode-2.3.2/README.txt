Recorded from real `opencode2 acp` runs (@opencode/cli 2.3.2, free model
opencode/deepseek-v4.1-flash-free) by driving the agent over stdio and logging
every JSON-RPC frame. The only edit is the workspace path, replaced by
/workspace. Do not hand-edit these; record a new run instead.

tools.jsonl  read, write, shell and a background subagent (OpenCode child-session tags)
perm.jsonl   OPENCODE_CONFIG_CONTENT asks for edit/shell: two session/request_permission
             requests, the edit with a diff
steer.jsonl  _session/steering during a running shell command ("injected")
subagents.jsonl  two background subagents in parallel; OpenCode sends no end for
             either, the parent continues after each child's final message
