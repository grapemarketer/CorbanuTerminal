# Screens

Text copies of real product screens captured on developer builds between 2026-10-14 and 2026-10-15, in disposable
profiles. They show what the pages look like; they come from earlier builds than the one under test, and some were
made against local stand-ins (noted below). Paths are shortened to `/Users/tester/run/workspace`.

| File | What it shows |
| --- | --- |
| s06-01-side-review-technical.txt | Technical page of an automatic-review request made inside `/side` (Z.AI) |
| s07-moonshot-k3-first-screen / -request / -technical | `/cost` for one `kimi-k3` turn on a person-configured provider `kimi` at api.moonshot.ai (replayed through a stand-in of real usage) |
| s07-moonshot-k2-first-screen / -technical | The same for `kimi-k2.7-code` |
| s07-corbanu-api-first-screen / -request / -technical / -other-conversations | A Corbanu API request priced from its reported charge |
| s07-vercel-first-screen / -request / -technical-attempt2 | A Vercel AI Gateway request priced from its reported charge |
| s08-restart-notice | The session right after restarting on an OpenAI conversation: the warm-up notice |
| s08-restart-first-screen / s08-restart-reopen-warmup-request | `/cost` after that restart: the reopen warm-up line and its request page |
| s08-resume-same-conversation | `/resume` of the conversation already open |
| s08-skip-reopen-screen / s08-skip-first-screen | Restart with `skip_reopen_warmup = true`, then `/cost` |
| s08-fast-tier-first-screen / -technical | An OpenAI priority-tier request with no published price (components with no rate) |
| s08-declared-basis-technical | A person-configured provider declared `billing = "pay_per_use"` |
| s09-chatgpt-catalogued / -uncatalogued / -priority-tier / -image first screens | `/cost` on a ChatGPT sign-in (stand-in) |
| s09-image-first-screen / -technical | An image request on an OpenAI API key |
