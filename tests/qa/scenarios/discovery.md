# Discovery Flow

## Summary

End-to-end test of the Discovery flow through Zulip only. Creates a project, starts a job, observes Intake and Evaluate stage agents on the project stream, and validates stage handoff. No API calls.

**Scope:** Full Discovery flow via Zulip (Concierge DM + stream observations + stage replies)
**Prerequisites:** None
**Mode:** zulip-only

---

**IMPORTANT: Stream Topic Rules (NON-NEGOTIABLE)**
- The stream topic is for STAGE AGENT communication ONLY. You are an observer who replies when asked.
- **NEVER** send unsolicited messages to the stream topic. NEVER request features, ask questions, or give instructions to TurboJET via the stream.
- You may ONLY send to the stream topic (`zulip_send.sh`) when DIRECTLY replying to a Stage agent's question. One reply per question. Do NOT send multiple messages while waiting for a response.
- All user commands (create project, start job, status checks, etc.) go to DM (`zulip_send_dm.sh`) ONLY.
- After sending a reply to a stage agent, WAIT for the agent's response before sending anything else. Do NOT send follow-ups while the agent is processing.
- Before sending to a stream topic, verify the topic belongs to the CURRENT run by checking the topic number.

---

**IMPORTANT: Answering Stage Agents (NON-NEGOTIABLE)**
- When a Stage agent asks you questions, you MUST answer EVERY question thoroughly and honestly.
- Provide detailed, complete answers. Do NOT give lazy one-liners like "Looks good" or "Proceed."
- If the agent asks about platform, features, tech stack, users, constraints — answer each one specifically.
- You are a cooperative user who wants the project to succeed. Never argue with or hold back information from the agent.
- Your answers should be detailed enough that the agent can proceed without re-asking the same question.
- Example of WRONG answer: "Looks good, proceed." (lazy, ignores the question)
- Example of RIGHT answer: "It's a web app targeting desktop browsers. Tech stack is HTML, CSS, JavaScript — no frameworks. Users are general public needing quick math. It should work on mobile too but desktop is primary." (addresses each point)

---

## Test 1: Concierge Greeting

Send a greeting to Concierge via DM.

**Send:**
```bash
./scripts/zulip_send_dm.sh "Hello!"
```

**Monitor for response (wait up to 30 seconds):**
```bash
timeout 30 ./scripts/zulip_monitor_dm.sh 5
```

**Expect:**
- Concierge responds with a human-readable welcome message.
- Response does NOT contain raw JSON, stack traces, or technical identifiers.

---

## Test 2: Verify No Existing Projects

Ask Concierge to list projects. Should be empty after a fresh setup.

**Send:**
```bash
./scripts/zulip_send_dm.sh "Which projects do I have available?"
```

**Monitor for response (wait up to 30 seconds):**
```bash
timeout 30 ./scripts/zulip_monitor_dm.sh 5
```

**Expect:**
- Concierge responds that no projects exist yet.
- Response is human-readable.

---

## Test 3: Create Project

Ask Concierge to create the test project.

**Send:**
```bash
./scripts/zulip_send_dm.sh "Create a project called test-project with repo https://github.com/example/calculator and base branch main."
```

**Monitor for response (wait up to 30 seconds):**
```bash
timeout 30 ./scripts/zulip_monitor_dm.sh 5
```

**Expect:**
- Concierge confirms project creation.
- Response mentions "test-project" and is human-readable.

---

## Test 4: Verify Project Created

Ask Concierge to list projects again. Should now show test-project.

**Send:**
```bash
./scripts/zulip_send_dm.sh "Which projects do I have available?"
```

**Monitor for response (wait up to 30 seconds):**
```bash
timeout 30 ./scripts/zulip_monitor_dm.sh 5
```

**Expect:**
- "test-project" appears in the project list.
- Response is human-readable.

---

## Test 5: Start Discovery Job

Ask Concierge to start a job.

**Send:**
```bash
./scripts/zulip_send_dm.sh "Start a new job in test-project. I need a calculator app."
```

**Monitor for response (wait up to 30 seconds):**
```bash
timeout 30 ./scripts/zulip_monitor_dm.sh 5
```

**Expect:**
- Concierge confirms the job was created.
- Response is human-readable.

**Capture the topic name if visible in Concierge's response or from the stream.** This is needed for subsequent tests.

---

## Test 6: Intake Stage Observation

Observe the Discovery flow starting and Intake stage agent engaging.

**Monitor the test-project stream (wait up to 120 seconds):**
```bash
timeout 120 ./scripts/zulip_monitor.sh test-project 5
```

**Expect:**
- "Discovery flow started" notification appears in a new topic (format: `Discovery #N`).
- "Intake stage started" notification appears in the same topic.
- The Intake agent posts a message asking clarifying questions about the calculator app.
- All messages come from the TurboJET bot.
- Messages are human-readable, not raw JSON.

**Record the exact topic name.** This is the run topic for all subsequent tests.

---

## Test 7: Answer Intake Agent

Reply to the Intake agent's question with detailed requirements.

**Precondition: Only send AFTER confirming the Intake agent posted a question. Verify the topic name matches the current run.**

**Send:**
```bash
./scripts/zulip_send.sh "Platform: Web app running in desktop browsers (Chrome, Firefox, Safari). Tech stack: Plain HTML, CSS, and JavaScript — no frameworks or libraries. Features: Basic arithmetic — addition, subtraction, multiplication, division. The UI should have a clean calculator layout with a display area and buttons for digits 0-9, decimal point, and the four operations. Target users: General public who need quick everyday math calculations. No authentication needed. Should work offline — no server-side components. Responsive design preferred but desktop is the primary target." "{topic_from_test_6}" "test-project"
```

**Monitor for response (wait up to 300 seconds):**
```bash
timeout 300 ./scripts/zulip_monitor.sh test-project 5
```

**Expect:**
- Intake agent responds with an acknowledgment, follow-up question, or requirements summary.
- Response stays in the same topic.
- Response is human-readable.

If the agent asks a follow-up question, answer it:
```bash
./scripts/zulip_send.sh "Division by zero should show an error message on the display. Support decimal numbers. Chain calculations should work — pressing an operator after a result continues the calculation. There should be a clear/reset button and a backspace button. That's all I have — please proceed with these requirements." "{topic_from_test_6}" "test-project"
```

Then monitor again for up to 300 seconds:
```bash
timeout 300 ./scripts/zulip_monitor.sh test-project 5
```

**Expect:**
- "Intake stage completed" notification appears in the topic.

---

## Test 8: Evaluate Stage Agent

After Intake completes, the Evaluate stage starts. Evaluate scrutinizes the Intake findings and asks about gaps, risks, and ambiguities. You must answer every question with specific, concrete details. This is NOT optional — the entire test depends on cooperative, thorough answers.

**Monitor the test-project stream (wait up to 120 seconds):**
```bash
timeout 120 ./scripts/zulip_monitor.sh test-project 5
```

**Expect:**
- "Evaluate stage started" notification appears in the same topic.
- The Evaluate agent posts questions about gaps, risks, ambiguities, or quality concerns.
- Evaluate builds on Intake's findings — does NOT re-ask the same basic questions.
- Messages are human-readable.

**Precondition: Only send AFTER confirming the Evaluate agent posted a question. Verify the topic name matches the current run.**

**Answer Evaluate's questions cooperatively.** Read each question carefully and provide a specific answer. You are a real client who wants this project to succeed — give clear decisions on every option presented.

**IMPORTANT:** Your answers must be DIRECT RESPONSES to what Evaluate asks. If Evaluate asks 9 questions with labeled options (A, B, C), answer each one. If Evaluate asks about specific technical decisions (operator precedence, floating point, etc.), give a concrete answer. Do NOT say "looks good" or "proceed" or "I trust your judgment" — those are non-answers that waste the test.

Use up to 3 rounds. Adapt your answers to what Evaluate actually asks:

Round 1:
```bash
./scripts/zulip_send.sh "Here are my answers to your questions:

1. Operator precedence: Option A — left-to-right immediate execution (calculator-style). When the user presses 2 + 3 × 4 =, the result should be 20, not 14. Show intermediate results after each operator press.

2. Floating point: Option C — use a precision threshold. Strip floating-point noise. 0.1 + 0.2 should show 0.3, not 0.30000000000000004. Round to 10 significant digits.

3. Division by zero post-error state: Option A — lock the calculator and require the user to press Clear to continue. Show 'Cannot divide by zero' on the display until Clear is pressed.

4. Negative numbers: Yes, include a +/- toggle button. Users should be able to directly enter negative numbers, not just get negative results from subtraction.

5. Display overflow: Option A — show in scientific notation for large results (e.g., 9.999e+29). Also cap input at 15 digits to prevent overflow from user input.

6. Repeated equals: Option B — pressing = multiple times just shows the same result. No repeat-last-operation behavior.

7. Keyboard input: Option A — yes, support keyboard input. Number keys, operators (+, -, *, /), Enter for equals, Escape for Clear, Backspace for CE.

8. Double decimal: Option A — ignore the second decimal press. If the user types 3.1. the second dot does nothing.

9. Deployment: Option A — just open index.html from the filesystem (file:// protocol). No server needed." "{topic_from_test_6}" "test-project"
```

Monitor for response (wait up to 300 seconds):
```bash
timeout 300 ./scripts/zulip_monitor.sh test-project 5
```

If Evaluate asks follow-up questions, answer them specifically. Read the questions and respond to each one:
```bash
./scripts/zulip_send.sh "Let me address your follow-ups:

- The display should show two lines: the running expression on top, the current value below. Like the Windows Calculator.
- No memory functions (M+, MR, MC) are needed for this version.
- No dark mode — light theme only, clean and simple.
- The calculator should handle numbers from -99999999999999 to 99999999999999 (15 digits).
- Error messages should appear in the display area, replacing the current value, in red text.
- No history feature needed — just the current calculation." "{topic_from_test_6}" "test-project"
```

Monitor for response (wait up to 300 seconds):
```bash
timeout 300 ./scripts/zulip_monitor.sh test-project 5
```

If Evaluate asks a third round, answer whatever remains:
```bash
./scripts/zulip_send.sh "That covers everything I need. The key priorities are: simple calculator-style left-to-right execution, clean floating-point display, keyboard support, and a two-line display. No frameworks, no server, no build tools — just plain HTML/CSS/JS that opens from the filesystem." "{topic_from_test_6}" "test-project"
```

Monitor for response (wait up to 300 seconds).

**Expect:**
- Evaluate processes each answer and either asks more questions or completes.
- "Evaluate stage completed" notification eventually appears.
- Evaluate's verdict should be 'proceed' (not 'reject') since all questions were answered.
- No error messages in the topic.

---

## Test 9: Stage Handoff Verification

Review all messages captured during the test. Verify:

1. **Intake → Evaluate handoff.** Evaluate had context from Intake. It did not re-ask basic questions.
2. **Topic discipline.** All messages stayed within the correct run topic. No leakage.
3. **No raw JSON or stack traces.** All agent messages were human-readable.
4. **Notifications were correct.** Each stage sent started/completed notifications.

---

## Cleanup

Cancel any active runs:
```bash
./scripts/zulip_send_dm.sh "Cancel any active runs in test-project"
```

Wait for confirmation.
