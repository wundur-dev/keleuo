# Game assets

Tools for making game assets with your own keys, for every agent in Keleuo.

- `make_image`: an image with OpenAI's gpt-image (directly, or through Azure OpenAI), saved as a PNG in `assets/generated/`, and shown to the agent.

Set your OpenAI key (or the Azure endpoint, deployment and key) in Settings > Tools > Packs. Each image asks before it runs (it costs money); "Always for this session" stops asking.
