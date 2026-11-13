# Proposal: a hosted asset service on Azure (images, 3D models, animation)

Status: draft for the owner, 2026-11-13. Nothing here is built yet.

## Why

Today Wundur's asset pipeline runs on the person's own keys: gpt-image for the concept and the
views, Tripo for the model and the rig. That works, but it puts setup in front of every new
person, and Tripo is the expensive step (roughly 30 cents to a dollar a model).

The idea: a service we run on Azure, behind a Wundur account, that does the same pipeline:

1. **Images** with gpt-image-2.5 on Azure (Microsoft Foundry).
2. **3D models** with Tripo *and* with an open model we host ourselves on Azure GPUs
   (TRELLIS.2 or Hunyuan3D), which should cost a fraction of Tripo per model.
3. **Rigging and animation**, ideally open source too (Mesh2Motion, or a GPU rigger).

If the open models are good enough for most game assets, a plan can offer much higher limits
for the same price than one built on Tripo, while costing us less.

## What it does, end to end

```
Wundur app ──(signed in)──> API (api.wundur.ai)
                             │
     ┌───────────────────────┼─────────────────────────┐
     ▼                       ▼                         ▼
 gpt-image-2.5          job queue ──> GPU worker     Tripo API
 (Azure Foundry)                      (TRELLIS.2 /   (fallback or
     │                                 Hunyuan3D,     "premium")
     │                                 rigging)
     └──────────────> Blob storage (images, GLB/FBX) <──┘
                      the app downloads by short-lived link
```

Images come back in seconds and can be answered directly. 3D and rigging take tens of seconds
to minutes, so they are **jobs**: the app asks, gets a job id, and polls (or is told) when it is
done. Everything Wundur's card already does fits this: it shows a spinner per step.

## 1. Images: gpt-image-2.5, and the one-image turnaround test

### Normal use

Concepts at low quality (cheap, fast), then views. Same as today, with our key instead of the
person's.

### The experiment: three views in one image

Instead of three separate requests for front, side and back, ask for **one image holding all
three**, laid out so it can be cut apart consistently:

- A wide canvas (for example 1536×1024), split into **three equal columns**: front, side
  (facing left), back, in that order.
- The same character in the same pose (A-pose or T-pose), the **same scale**, feet on the
  **same baseline**, centred in each column, nothing crossing a column's edge.
- A plain background (or transparent), with clear empty space between the figures.
- No text, labels, shadows or props.

Why it might be *better* than three requests, not just cheaper: one generation draws all three
views at once, so they should agree with each other (the same armour, colours and proportions).
Three separate requests drift, and mismatched views are what make multiview 3D fail.

How we split it, most robust first:

1. **Find the figures, don't trust the grid:** with a transparent or plain background, find
   each figure's bounding box (alpha or background difference) and cut around it.
2. **Fall back to fixed thirds** when that fails.
3. **Normalize each view:** pad to a square, same figure height in each, same baseline, ready
   for the 3D step.

What to measure (20 characters: humanoids, creatures, props):

| Check | Pass when |
|---|---|
| Layout followed | three figures, in the right order, in their columns, ≥ 90% of the time |
| Split cleanly | the automatic split needs no fixing ≥ 90% of the time |
| Views agree | a person judges front/side/back to be the same character |
| 3D result | a model from the split views is at least as good as from three separate images |

Cost: one image instead of three, so roughly a third of the views step.

## 2. 3D models: Tripo and an open model on Azure GPUs

### The candidates

| | TRELLIS.2 (Microsoft) | Hunyuan3D 2.x (Tencent) | Tripo (today) |
|---|---|---|---|
| Input | one image | one image, or **front/side/back** (its multiview model) | one image or multiview |
| Output | textured GLB | mesh + texture | textured GLB, rig, animation |
| Hosting | ours, GPU | ours, GPU | their API |
| Licence | MIT (to confirm) | Tencent community licence: **not usable in the EU, UK and South Korea**, and has a cap on monthly users (to confirm) | commercial, per call |
| Cost per model | GPU seconds (cents) | GPU seconds (cents) | about 30¢ to $1 |

The licence matters more than the quality here. Hunyuan3D's terms may rule it out for a
service anyone can sign up to; TRELLIS.2's (if MIT, as I believe) don't. **Check both licences
before any work.**

Hunyuan3D's multiview model takes front, side and back directly, which matches our views step.
TRELLIS.2 takes one image, so it would get the front view (or the concept), and the side and
back views would only be for checking.

### Running it on Azure

- **Azure Container Apps with serverless GPUs** (A100 or T4; A100 for these models), billed per
  second while running, and able to **scale to zero** when idle.
- The catch is **cold start**: these models are many gigabytes, so the first job after a quiet
  spell waits minutes while a container starts and loads them. Because 3D is already a job the
  person waits for, that may be acceptable; if not, keep one worker warm during the hours people
  use it.
- A queue (Azure Storage queue or Service Bus) feeds the GPU worker; the worker scales on queue
  length, so a burst of jobs gets more GPUs and a quiet night gets none.

### Rough cost (to be measured, not trusted)

- A100 serverless is about **$1.90 an hour while running** (third-party price trackers, autumn
  2026; check Azure's own page for the region).
- If a model takes about a minute of GPU time, that's **about 3¢ a model**, against Tripo's
  30¢ to $1.
- A worker kept warm all month is about **$1,400**, roughly the same as 2,000–4,500 Tripo
  models. So: scale to zero until there's steady volume, and warm only once it pays.

The numbers that decide this, measured on the same 20 characters:

| Measure | Why |
|---|---|
| GPU seconds per model | the cost per model |
| Cold start time | whether scale-to-zero is acceptable |
| Quality against Tripo, judged blind | whether it is "good enough for most games" |
| Mesh size and topology | whether it rigs and animates |

### How a plan could use both

The open model as the default (higher limits), Tripo as the "best quality" option with its own
smaller limit, or as the fallback when the open model fails a model's check.

## 3. Rigging and animation: Mesh2Motion, or a GPU rigger

**Mesh2Motion** is, as far as I know, an open-source Mixamo-style tool that runs **in the
browser** (three.js): it fits a skeleton template (human, and some animals) to a model and
applies animations. If that's right, it doesn't need to live in the GPU container at all: it
could run in Wundur's own web panel, next to the 3D viewer, at no cost to us. That's worth
confirming first, because it changes the design.

If we want rigging on the server instead (automatic, no person in the loop), the candidates are:

- **Tripo's rig** (what Wundur uses today; paid per call).
- **An open auto-rigger on the GPU worker**, such as UniRig (licence and quality to check).

The same container *can* host the 3D model and a rigger (one image, one GPU, two jobs), but
it's simpler to keep them as separate job types on the same worker pool, so either can change
without touching the other.

## The rest of the service

- **API:** a small web service on Azure Container Apps (no GPU), at `api.wundur.ai`.
  Endpoints: images (generate, edit), jobs (model, rig; create, status, result), the person's
  allowance (`/v1/me`), and later summaries.
- **Accounts:** Microsoft Entra External ID; the app signs in with a browser and a loopback
  redirect (PKCE) and keeps its token in the Keychain.
- **Limits, enforced on the server:** images per hour and per day, models one at a time and per
  month, failed jobs not counted.
- **Keys and secrets:** Key Vault, reached by managed identity; nothing secret in the app.
- **Storage:** Blob storage for outputs, handed to the app as short-lived links; kept 30 days.
- **Records:** jobs, accounts and usage in Postgres (or Cosmos DB).
- **Watching it:** Application Insights; an alert when the GPU queue backs up or costs jump.
- **Infrastructure as code:** Bicep, deployed from GitHub Actions.
- **Content safety:** Azure's filters on for images; prompts and outcomes logged for review.

## Order of work

1. **Experiments first, on a laptop or one GPU VM:** the one-image turnaround test, TRELLIS.2
   and Hunyuan3D on the same 20 characters, and Mesh2Motion in the browser. A week or two, a
   few dollars of GPU time. This decides everything after it.
2. **Licences** for whichever 3D model wins.
3. **The API with images only** (gpt-image through our key), accounts and limits.
4. **3D jobs** on serverless GPU, scale to zero.
5. **Rigging**, in whichever form the experiments pointed to.

## Questions for the owner

1. **How many people, roughly, in the first months?** A handful (family, friends, a game jam)
   or open sign-up? It decides whether scale-to-zero and its cold starts are fine.
2. **Is waiting a few minutes for a model acceptable** the first time after a quiet spell, or
   should models always come back within a minute or so (which means a warm GPU and a monthly
   cost)?
3. **Should Tripo stay** as a "best quality" option, or be dropped once an open model is good
   enough?
4. **Where are the users?** If any are in the EU or UK, Hunyuan3D is likely out.
5. **Rigging in the app (Mesh2Motion in the web panel, the person places markers) or fully
   automatic on the server?** The first is free and works today's way; the second is hands-off.
6. **Azure region and quota:** the GPU quota has to be asked for in a region (the same quota
   process as gpt-image). Which region, and are the credits on a subscription that can get GPU
   quota at all?

## Facts to confirm before relying on them

These are from memory or third-party pages, not checked against the sources:

- gpt-image-2.5's availability and quota on Azure in our region.
- TRELLIS.2's licence, GPU memory needs and seconds per model.
- Hunyuan3D's licence terms (regions, user cap) and its multiview model's inputs.
- Mesh2Motion: that it runs entirely in the browser, its licence, and which skeletons it has.
- Azure Container Apps serverless GPU: which GPUs in which regions, per-second prices, and
  cold-start behaviour for large images.
