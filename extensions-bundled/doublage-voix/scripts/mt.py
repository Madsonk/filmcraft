#!/usr/bin/env python3
"""Offline machine translation helper of the Doublage vocal extension (Opus-MT / Marian).

    python mt.py <model folder>   < {"texts": [...], "nbest": 1}   > {"out": [[candidate, ...], ...]}

The folder holds config.json, vocab.json, source.spm, target.spm and model.safetensors (the files
FilmCraft's `transcript.downloadModel {model:"opus-mt-tc-big-fr-en"}` installs). Needs torch, transformers
and sentencepiece; runs on the CPU, nothing is sent anywhere.
"""
import json
import sys

import torch
from transformers import MarianMTModel, MarianTokenizer


def main():
    req = json.load(sys.stdin)
    d = sys.argv[1]
    tok = MarianTokenizer.from_pretrained(d)
    model = MarianMTModel.from_pretrained(d).eval()
    nbest = max(1, int(req.get("nbest", 1)))
    res = []
    for text in req["texts"]:
        enc = tok([text], return_tensors="pt", padding=True)
        with torch.no_grad():
            gen = model.generate(**enc, num_beams=max(4, nbest), num_return_sequences=nbest, max_new_tokens=256)
        res.append([t.strip() for t in tok.batch_decode(gen, skip_special_tokens=True)])
    json.dump({"out": res}, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
