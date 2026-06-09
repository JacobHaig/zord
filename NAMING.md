# Product Naming — Candidate List & Rename Rationale

## What this file is

This is the **working document for renaming Zord** before it goes commercial. It exists so the naming decision can be reviewed asynchronously — pick it up any time, react to candidates (keep / kill / riff), and shortlist 3–5 names for formal verification. Nothing here is final; **every name below is unverified** unless its Status column says otherwise.

### Why we're renaming at all

**"ZORD" is a live registered US trademark** — Reg. 5124465, owned by SCG Power Rangers LLC (a Hasbro entity since 2018), covering toy classes. MEGAZORD has been registered since 1993 and already covers "card games and hand held games." The portfolio (~280 marks) is actively defended, and Hasbro litigates.

The risk is specific to our plan, not abstract:

1. **We intend to sell on Steam** — a gaming storefront where Power Rangers titles are actively sold. Marketplace proximity strengthens a likelihood-of-confusion claim even across trademark classes.
2. **"Zord" is a coined word with no independent meaning** — there is no "we mean something else" defense; the only association anyone has is the franchise.
3. **Famous marks get dilution protection beyond their registered classes.**
4. The cheap time to rename is **now** — before the domain purchase, Steam page, site brand, and signed binaries. A cease-and-desist mid-launch would force all of that under pressure.

### How a name gets chosen (the verification gauntlet)

When 3–5 candidates are shortlisted, each goes through:

- [ ] **USPTO search** (TESS/TSDR) — live marks in Class 9 (software) and Class 42 (SaaS), plus famous-mark adjacency
- [ ] **Domain availability** — `.app`, `.com`, `.gg`, `.io` (own-domain decision is already made)
- [ ] **Marketplace collisions** — Steam search, Apple/Microsoft app stores, Product Hunt
- [ ] **Developer-namespace collisions** — GitHub orgs/repos, crates.io (the product is Rust), Homebrew/winget package names
- [ ] **Search-engine test** — does page 1 of results belong to someone else's brand?
- [ ] **Mouth test** — easy to say aloud, unambiguous to spell when heard, works as a verb if relevant ("just zord it")

Survivors get scored; the winner goes into the platform game-plan doc and the rebrand happens in one sweep (crate names, app bundle IDs, repo, site SPEC, Steam page).

### Naming context (what the product is)

Private, fully-local meeting transcription: records mic + system audio, transcribes on-device (Whisper/Parakeet), optional local-AI summaries and chat. **Nothing leaves the machine.** Target buyer skews **gamer / technical** — comfortable with comms, logs, sitreps, and tools that respect them. The name should ideally signal one of: *privacy, capture/record, voice/speech, memory* — or just be ownable and fun.

> ### ⚠️ Action item — fold this process into WebSmith
>
> Everything this document does — trademark exposure check, collision sweep, the verification gauntlet, naming-before-brand-spend — happened **ad hoc** for this product. It must become a **standard step in the WebSmith site process** (agent-cloud: `agents/websmith/`), so every future site/product spec starts with name clearance *before* SPEC.md signing, domain purchase, or brand work. Update the WebSmith prompt/recipe to include a "Naming & trademark clearance" gate mirroring this file's gauntlet. (Tracked alongside the platform game plan.)

---

## Candidates

Legend — **Tone:** does it keep Zord's punchy sci-fi energy? · **Status:** `unverified` = gut-check only; `⚠️` = known or suspected collision worth checking first; `❌` = known burned, kept for reference.

### Bucket 1 — Keeps the Zord energy (short, punchy, consonant-heavy, coined)

| Name | Why it works / how it'd be used | Status |
|---|---|---|
| **Zylo** | Z-brand, two syllables, gadget-like. "Open Zylo," "Zylo it." Easy to say in any language. | unverified |
| **Zeph** | From *zephyr* — light, passes through unnoticed. Privacy implication inside a Z-package. | unverified |
| **Vorn** | The same hard sci-fi punch as Zord with no franchise attached. Reads like good hardware. | unverified |
| **Zekt** | Abrupt, mechanical, memorable. Pure coined word — maximum trademark safety if clear. | unverified |
| **Zindle** | Z + kindle/spindle. Quirky and warm; slightly toy-like (which may be wrong for "sell to professionals"). | unverified |
| **Quorz** | Quartz with the edges filed off. Crystal = stored memory imagery. | unverified |
| **Zorvan** | Echo of Zurvan (Zoroastrian deity of *time* — fitting for timestamped records). Keeps the Z+r+v feel. | unverified |
| **Vantor** | Coined, sturdy, enterprise-credible. Sounds like it has an SLA. | unverified |
| **Zelt** | German for "tent" — a private shelter, your own walls. Short and stampable. | unverified |
| **Drax** | ❌ Guardians of the Galaxy character — kept here as the example of the failure mode we're avoiding. | ❌ burned |

### Bucket 2 — Completely different tone (clean, brandable, meaning built later)

| Name | Why it works / how it'd be used | Status |
|---|---|---|
| **Quillside** | Writing + "your side of the desk." Zero collisions found in the first sweep — currently the safest known candidate. | unverified (1 clean pass) |
| **Loma** | Short, warm, pronounceable everywhere. The anti-Zord. | unverified |
| **Halden** | Trustworthy Nordic safe-deposit-box energy. | unverified |
| **Bracken** | Grounded nature word; dense undergrowth = good cover (a privacy wink). | unverified |
| **Norrel** | Invented and soft; faint *Jonathan Strange & Mr Norrell* echo (book, unrelated class). | unverified |
| **Calder** | Cold mountain stream; clean, masculine, brandable. | unverified |
| **Fenwick** | English place-name charm; "from the fen" — murky, hard to see into. | unverified |
| **Tarn** | A small, deep mountain lake — contained, still, holds everything dropped into it. One syllable. | unverified |
| **Cairn** | A stack of stones marking *what happened here* — literally a memory marker. Strong metaphor for meeting records. | ⚠️ used by some tech projects — check |
| **Holt** | An otter's den (!) — the private place. Sly nod to Otter.ai while being its opposite (local, hidden). One syllable. | unverified |
| **Skein** | A tangle of thread wound into order — exactly what transcription does to a conversation. | unverified |

### Bucket 3 — Mimics the use case (capture, transcript, memory)

| Name | Why it works / how it'd be used | Status |
|---|---|---|
| **Retell** | What it does: the meeting, retold. Verb-able ("retell yesterday's standup"). Clear instantly. | unverified |
| **Quoth** | Archaic "said" (*quoth the raven*). Literary, fun, and literally about attributing speech — which is the diarization feature. | unverified |
| **Hearsay** | What others said, reported. Legal-term irony (hearsay is *inadmissible* — but yours is verbatim). Memorable. | ⚠️ common word — domain/mark check |
| **Minutia** | Meeting **minutes** + the small details it captures. Clever double meaning; slightly hard to spell. | unverified |
| **Aside** | Theater term: a line spoken privately. Short, elegant, dual meaning (private + spoken). | ⚠️ very common word |
| **Earshot** | "Within earshot" — everything you could hear, kept. Audio + proximity + a little edge. | ⚠️ likely some usage — check |
| **Eaves** | The root of *eavesdrop* — short, wry, self-aware. (It eavesdrops on your own meetings, for you.) | unverified |
| **Keeplog** | Keep + log, and *keep* as in castle keep — the fortified store. | unverified |
| **Inkwell** | Warm, archival, evokes the transcript. | ⚠️ common word — expect squatting |
| **Vellum** | Premium archival writing surface. Feels expensive (good for one-time purchase pricing). | ⚠️ prior app usage — check |
| **Notary** | The trusted third party who witnesses and records — except it's *you*. Trust + record in one word. | ⚠️ regulated-profession adjacency |

### Bucket 4 — Privacy idiom (the product promise as a name)

| Name | Why it works / how it'd be used | Status |
|---|---|---|
| **Subrosa** | *Sub rosa* — "under the rose," told in confidence. The strongest meaning-fit in this file. | ⚠️ idiom is used by scattered products — check |
| **Fourwalls** | "What's said within these four walls…" — instantly communicates local-only. | unverified |
| **Entrenous** | *Entre nous* — "between us." Elegant, conspiratorial. Spelling may fight us. | unverified |
| **Privy** | "Privy to the conversation." Short, exact. ⚠️ Also British slang for an outhouse — focus-group it. | ⚠️ double meaning |
| **Enclave** | Private territory; also the literal term for a secure compute region (SGX/TrustZone) — the technical audience will catch it. | ⚠️ overloaded in security branding |
| **Vaulted** | Stored in the vault, and "vaulted" as in leapt — a little motion in it. | unverified |
| **Offrecord** | "Off the record" — except it *is* the record, kept off everyone else's servers. Nice tension. | unverified |

### Bucket 5 — Gamer / comms flavor (speaks the target audience's language)

| Name | Why it works / how it'd be used | Status |
|---|---|---|
| **Debrief** | The post-mission report — *exactly* what the app produces from a meeting. Military/gamer native vocabulary, professional enough for work. Verb-able. | ⚠️ common word; likely product usage — check hard, worth it |
| **Sitrep** | "Give me a sitrep" = catch me up = the summaries/chat feature, verbatim. Punchy, known to every gamer. | ⚠️ check games/tools usage |
| **Callout** | In comms, a *callout* is the critical info someone shouts. The app catches every callout. | ⚠️ common word |
| **Backchannel** | Side comms + the diplomatic "private channel." Double meaning lands perfectly for private transcription. | ⚠️ known term in tech — check |
| **Comlog** | Comms log; ship's-log sci-fi undertone. Low collision risk, easy `.app`. | unverified |
| **Sidelog** | The quiet log running alongside the call. | unverified |
| **Squawk** | Radio/aviation: the squawk box, squawk codes. Loud word for a quiet product — fun tension. | ⚠️ aviation/finance products use it |

### Bucket 6 — Fun to say (mouth-feel first)

| Name | Why it works / how it'd be used | Status |
|---|---|---|
| **Yaplog** | "Yap" is *the* current slang for talking too much — the app logs the yapping. Gen-Z/gamer native; ages with the slang, though. | unverified |
| **Chinwag** | British for "a good chat." Genuinely funny, warm, memorable; maybe too cheeky for HR/legal buyers. | unverified |
| **Natter** | British "to chat away." Friendly, light, two syllables. | unverified |
| **Hubbub** | The noise of many voices — which it untangles. Bouncy, doubled syllable, sticks in the head. | ⚠️ a comms product used this — check |
| **Confab** | A confidential talk, literally (*con- fabulari*). Fun AND etymologically private. Underrated. | unverified |
| **Parley** | Negotiated talk under truce; pirate-flavored, gamers know it. | ⚠️ some app usage — check |
| **Gabble** | Rapid talk; goofy-charming. | unverified |
| **Blathercatch** | It catches blather. The silliest thing in this file; kept because memorable beats forgettable. | unverified |
| **Jotter** | British for notebook; bouncy and verb-able. | ⚠️ generic-word collisions |
| **Parlo** | Esperanto "I speak." Snappy, international. | unverified |
| **Squib** | A small firework that doesn't make a big bang — quiet by design. (Common word predating Harry Potter's use.) | unverified |

---

## Known-burned names (do not revisit)

| Name | Why it's out |
|---|---|
| **Zord** | Live Hasbro/SCG trademark (Reg. 5124465) + famous-mark dilution + we'd sell on a gaming storefront. The reason this file exists. |
| **Sotto** | Existing local-Whisper transcription app (GitHub: Maciejonos/sotto). Same niche. |
| **Murmur / Murmure** | Two existing local speech-to-text products, plus Mumble's server is named Murmur. The "quiet word" space is crowded. |
| **Tacet** | Phonetically adjacent to **Tactiq**, a direct meeting-transcription competitor. |
| **Mumble / Echo / Otter / Whisper / Rewind / Recall / Grain / Fathom / Krisp / Parsec** | All established products in audio/meetings/gaming-adjacent software. |

---

## Current instinct shortlist (no decision yet)

From the first review round: **Subrosa** (meaning), **Quillside** (safest), **Debrief** (audience-fit, pending collision check), **Holt** (sly + short), **Zylo** (tone-keeper). Operator reaction pending — edit freely.

## Next steps

1. **Operator review** — mark keeps/kills directly in the tables (or just reply with reactions); ask for more rounds in any bucket.
2. Shortlist **3–5** → run the full verification gauntlet on each.
3. Score, pick, record the decision + evidence here.
4. Carry the winner into the platform game-plan doc (storefront, domain, Steam page, crate/binary rename sweep).
5. **Do the WebSmith update** (boxed action item above) so this process is a built-in gate for every future site.

## Revision history

| Date | Change |
|---|---|
| 2026-06-07 | Initial naming document: rename rationale (ZORD trademark findings), verification gauntlet, ~55 candidates across six buckets, burned list, WebSmith action item. |
