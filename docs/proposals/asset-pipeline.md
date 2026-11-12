# Proposal: the asset pipeline (image → model → rig, with the person approving each step)

Status: agreed with the owner 2026-11-12; built in phases. Written 2026-11-12.

## The idea in one paragraph

Game assets made with AI are mostly made badly: text-to-3D fails half the time, and every failed
try is paid for. The reliable path is the one the owner standardized for his son: iterate on an
**image** cheaply at low resolution (sending the last image back with notes) until it's right,
render **three views** (front in an A-pose, side, back) at medium resolution, turn those into a
**3D model** (Tripo, multiview), and **rig** it (Mixamo for humanoids, free and better than the
others; Tripo's rigging for creatures). Keleuo makes that path the default, native to the app:
each stage shows what was made, the person approves or asks for another try, and only approved
results move on. Every attempt is recorded with what it cost, so the cost of an *approved* model
is known (the basis of a later paid tier: N approved models a month, see Pricing).

## The stages

An **asset** (a knight, a T-rex, a chest) lives in the project at `assets/<kind>/<name>/` with
its record, `asset.json`: its brief, every attempt (what was asked, what it cost, the files),
and which result was approved at each stage.

1. **Concept.** A prompt (and optional reference images, and the project's style, see Style).
   Low-resolution images (gpt-image, `quality: low`), shown side by side; the person picks one
   and writes what to change; the next try sends that image back with the note (gpt-image edits
   keep it consistent). Approve when it's right. Typically 3 to 15 cents.
2. **Views.** From the approved concept: front (A-pose for a character), side and back, at
   medium resolution, each approvable or redone alone. About 30 cents.
3. **Model.** The views go to Tripo's multiview-to-model (single-image when there's only one);
   the GLB shows in the 3D viewer, turned and lit; approve, or try again (a try counter shows,
   with its cost). 30 cents to $1 a try.
4. **Rig and animate.**
   - **Humanoid: Mixamo, guided.** Keleuo asks Tripo for the model as FBX (what Mixamo takes),
     opens Mixamo in the web panel under the person's own Adobe sign-in, and shows a short
     checklist (upload, place the markers, pick animations, download "with skin"). Keleuo never
     drives or reads Mixamo's page: it only handles the download, through the web view's own
     download hooks, saving the file into the asset's folder under a proper name and showing it
     in the viewer. Free.
   - **Creature: Tripo's rig check, rig and presets** (`rig_model`), in the viewer. Paid.
5. **Done.** The approved files are in the asset's folder; the agent is told where, to wire them
   into the game (prefab, controller, scene).

Each stage can be redone later without losing the approved one (attempts are kept).

## What the person sees

An **Assets** panel: the project's assets as cards (name, stage, thumbnail, cost so far). An
asset opens to its current stage: the tries in a row, the chosen one large (an image, or the 3D
viewer), a field for what to change, and **Approve**, **Try again** and **Back**. Agents can
start an asset and fill its brief through a tool (`new_asset`), so "make me a knight enemy"
from the chat lands as an asset waiting for the person's choices: the person stays the one who
approves.

## Under it

- Services behind each stage are adapters, so the same stage runs on OpenAI or Azure, Tripo or
  another model maker, or local models (ComfyUI, later), and later a hosted tier.
- The 3D viewer (`view_model`'s page, three.js) in a web panel, with an FBX loader beside the
  GLB one for Mixamo's files.
- Downloads in the web panel (wry's download handlers): an asset waiting on Mixamo takes the
  next FBX downloaded; any other download goes to Downloads with a Move into project button.
- The costs are each service's price per call, kept per attempt in `asset.json`.

## Pricing, later

The owner's plan: a paid tier of N **approved, complete** models a month ($20, $40, $100), with
Keleuo carrying the failed tries; the image-first path keeps that cost low. From his numbers:
about $0.63 an approved model when it goes well, $1.00 to $1.40 typically, about $3.75 at a cap
of three tries on the $1 model. Tries are capped per stage so a model's cost has a ceiling. The
records above are what price it: measured on real use before any tier is sold. A paid tier
needs a small server holding the service keys, accounts and billing, and a check of the
services' terms on resold output; none of that is in this first build, which runs on the
person's own keys.

## Phases

1. **Records and the image stages:** `asset.json`, the Concept and Views stages with
   feedback iterations, the Assets panel, costs kept.
2. **Model stage:** Tripo multiview-to-model, the viewer in the stage, tries and costs.
3. **Rig stage:** web panel downloads, the guided Mixamo step with auto-import, FBX in the
   viewer; Tripo rigging for creatures.
4. **Agents:** `new_asset`, and the agent told where approved files are.
5. **Style** (a project's style kept once, sent with every image) and **local models**.

## Questions

1. Where assets live by default: `assets/<kind>/<name>/`, or the engine's own folder (Unity's
   `Assets/`, Godot's `res://`)? Probably the engine's, once there's an engine bridge.
2. Whether the Assets panel replaces the pack's tools for agents or sits beside them.
