# ADR 0106: Profile Skill selection and explicit composer context

- Status: accepted
- Date: 2026-09-09

The filesystem Skills Plugin owns discovery, multi-directory precedence, filtering, and all Skill reads. Profiles optionally store an allowed_skills list; omission inherits all Skills, an empty list selects none. The Host materializes this into Plugin configuration before Ready Gate. Disabled Skills are absent from prompts, tools and suggestions, not merely hidden by Console.

The Web surface accepts additive context_references identifying existing Context Source prompts/resources. It resolves them through the bound Capability before a Turn and retains the original user input separately for approval. References cannot nominate arbitrary filesystem reads. File and folder attachments remain explicitly selected browser uploads. Console stores Markdown as the draft/message interchange format; its rich editor is presentation, not a second message format.

When context expands model input, a Host-owned TurnInputPresentation extension records the original display_input alongside the full durable input. History renders the original text; model replay retains the selected context. Approval continues to evaluate the original user request.
