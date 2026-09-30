"""openai/whisper-small as three static-shape graphs for an NPU.

encoder: log-mel -> hidden states. cross: hidden states -> every decoder layer's cross-attention
K and V, once per turn. decoder: one token at one position, with the self-attention cache passed in
(448 slots) and the token's new K and V passed out; the caller writes them into the cache.

Masks use a large finite negative, not -inf: -inf is an fp16 hazard on the HTP (phase 0 findings).
"""
import torch
from transformers import WhisperForConditionalGeneration

WHISPER = "openai/whisper-small"
WHISPER_REVISION = "973afd24965f72e36ca33b3055d56a652f456b4d"
CACHE = 448
MASKED = -1e4

def load() -> WhisperForConditionalGeneration:
    return WhisperForConditionalGeneration.from_pretrained(WHISPER, revision=WHISPER_REVISION).float().eval()

def _heads(x, heads):  # [1, T, d] -> [1, H, T, Dh]
    b, t, d = x.shape
    return x.view(b, t, heads, d // heads).transpose(1, 2)

class Encoder(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.encoder = model.model.encoder

    def forward(self, features):
        return self.encoder(input_features=features).last_hidden_state

SHORT = 500  # encoder positions in the short set: ten seconds

class ShortEncoder(torch.nn.Module):
    """The encoder on the first 2n mel frames with the first n position embeddings: whisper.cpp's
    audio_ctx. transformers' encoder insists on 3000 frames, so its layers are run here directly."""
    def __init__(self, model, n):
        super().__init__()
        self.encoder, self.n = model.model.encoder, n

    def forward(self, features):
        e = self.encoder
        x = torch.nn.functional.gelu(e.conv1(features))
        x = torch.nn.functional.gelu(e.conv2(x)).permute(0, 2, 1) + e.embed_positions.weight[: self.n]
        for layer in e.layers:
            x = layer(x, attention_mask=None)
            x = x[0] if isinstance(x, tuple) else x
        return e.layer_norm(x)

class Cross(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        self.layers = model.model.decoder.layers
        self.heads = model.config.decoder_attention_heads

    def forward(self, hidden):
        ks = [_heads(layer.encoder_attn.k_proj(hidden), self.heads) for layer in self.layers]
        vs = [_heads(layer.encoder_attn.v_proj(hidden), self.heads) for layer in self.layers]
        return torch.stack(ks), torch.stack(vs)

class Decoder(torch.nn.Module):
    def __init__(self, model):
        super().__init__()
        d = model.model.decoder
        self.embed, self.positions, self.layers, self.norm = d.embed_tokens, d.embed_positions, d.layers, d.layer_norm
        self.proj = model.proj_out
        self.heads = model.config.decoder_attention_heads
        self.scale = (model.config.d_model // self.heads) ** -0.5
        self.register_buffer("slots", torch.arange(CACHE, dtype=torch.int32), persistent=False)

    def _attend(self, q, k, v, mask):
        w = torch.softmax(torch.matmul(q * self.scale, k.transpose(-1, -2)) + mask, dim=-1)
        out = torch.matmul(w, v)  # [1, H, 1, Dh]
        return out.transpose(1, 2).reshape(1, 1, -1)

    def forward(self, token, position, self_k, self_v, cross_k, cross_v):
        # An int32 embedding lookup for both: indexing the table with `position.long()` exported as
        # int64 wrap-around arithmetic the HTP does not run, splitting the decoder in two per step.
        x = self.embed(token) + torch.nn.functional.embedding(position, self.positions.weight).unsqueeze(0)
        here = (self.slots == position).to(x.dtype).view(1, 1, CACHE, 1)
        mask = torch.where(self.slots <= position, 0.0, MASKED).to(x.dtype).view(1, 1, 1, CACHE)
        new_k, new_v = [], []
        for i, layer in enumerate(self.layers):
            h = layer.self_attn_layer_norm(x)
            q = _heads(layer.self_attn.q_proj(h), self.heads)
            k = _heads(layer.self_attn.k_proj(h), self.heads)
            v = _heads(layer.self_attn.v_proj(h), self.heads)
            keys = self_k[i] * (1 - here) + k * here
            values = self_v[i] * (1 - here) + v * here
            x = x + layer.self_attn.out_proj(self._attend(q, keys, values, mask))
            h = layer.encoder_attn_layer_norm(x)
            q = _heads(layer.encoder_attn.q_proj(h), self.heads)
            x = x + layer.encoder_attn.out_proj(self._attend(q, cross_k[i], cross_v[i], 0.0))
            h = layer.final_layer_norm(x)
            x = x + layer.fc2(layer.activation_fn(layer.fc1(h)))
            new_k.append(k)
            new_v.append(v)
        logits = self.proj(self.norm(x))[:, 0, :]
        return logits, torch.stack(new_k), torch.stack(new_v)
