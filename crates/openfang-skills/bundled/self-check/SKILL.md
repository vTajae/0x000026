---
name: self-check
description: Reflexion-lite pattern for self-verification using agent_delegate
---
# Self-Check (Reflexion-Lite)

When completing complex or high-stakes tasks, use this self-check pattern to verify your work before delivering a final answer.

## When to Self-Check

- **Complex reasoning**: multi-step logic, math, code generation, or planning
- **High stakes**: tasks where errors have significant consequences
- **Uncertainty**: when you are unsure about correctness
- **User request**: when the user explicitly asks you to verify

## When to Skip

- Trivial lookups, greetings, or simple factual answers
- Tasks where the answer is obviously correct
- Follow-up clarifications on already-verified work

## How to Self-Check

Use the `agent_delegate` tool to invoke a fresh-context review of your work. The delegated agent receives only the review prompt — no accumulated session history — ensuring an independent assessment.

### Step 1: Complete your initial work

Produce your draft answer or solution as normal.

### Step 2: Delegate a review

Call `agent_delegate` with:
- `agent_id`: your own agent name or ID (self-delegation)
- `task`: a review prompt containing your draft output and explicit review criteria

Example review prompt:
```
Review the following solution for correctness, completeness, and edge cases.

SOLUTION:
[your draft output here]

ORIGINAL TASK:
[the user's original request]

Check for:
1. Logical errors or incorrect assumptions
2. Missing edge cases
3. Factual accuracy
4. Completeness relative to the original request

Respond with either "APPROVED" if the solution is correct, or provide specific corrections.
```

### Step 3: Incorporate feedback

- If the review returns "APPROVED", deliver your original answer.
- If corrections are suggested, revise your answer and optionally run one more review.
- **Maximum 2 review iterations** to prevent infinite loops.

## Example Flow

1. User asks: "Write a function to detect cycles in a directed graph"
2. You write the function (draft)
3. You call `agent_delegate` with a review prompt including your code
4. Reviewer finds: "Missing visited-set reset between components"
5. You fix the bug and deliver the corrected version

## Guidelines

- Keep review prompts focused — include only the draft output and review criteria
- Do not include your entire conversation history in the review prompt
- Trust the reviewer's independent assessment — it has fresh context
- If two review rounds still find issues, deliver your best version with caveats noted
