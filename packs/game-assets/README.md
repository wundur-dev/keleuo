# Game assets

Tools for making game assets with your own keys, for every agent in Keleuo. Each service's tools
appear once its key is set in Settings > Tools > Packs; everything is saved in
`assets/generated/` (a setting).

**Images: OpenAI's gpt-image, directly or through Azure OpenAI**

- `make_image`: an image from a description, saved as a PNG and shown to the agent.
- `edit_image`: change an image in the project (recolour, add, remove, restyle), optionally with a mask and reference images.

**Sound: ElevenLabs**

- `sound_effect`: a sound effect from a description, up to 30 seconds, optionally looping.
- `voice_line`: a line spoken by a voice; `list_voices` shows the voices the account has.
- `music`: a track from a description. ElevenLabs' terms limit music in games to plans that include it: check yours before shipping.

**3D models: Tripo**

- `model_from_text`, `model_from_image`: a textured GLB, with a preview image the agent can look at. A job of a minute or a few.

Tools that cost money ask before they run; "Always for this session" stops asking.
