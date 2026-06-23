---
name: compliant-position
description: Skill that runs zbrain search before external lookup
triggers:
  - "look up a topic"
---

# compliant-position

This skill demonstrates position-relative compliance: the first brain
reference (zbrain search) appears strictly before the first external
reference (web_search), so the analyzer accepts it without requiring
the canonical callout.

## Workflow

1. Run `zbrain search "topic"` to find existing brain pages.
2. If brain answer is thin, fall back to web_search for fresh data.
