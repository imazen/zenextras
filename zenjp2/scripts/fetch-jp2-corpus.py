# Fetch the JPEG 2000 files hayro-jpeg2000 lists in its manifests (OpenJPEG conformance, serenity) from
# hayro-assets.dev into a scratch directory, for `just inventory-oracle`. Usage: fetch-jp2-corpus.py [hayro-jpeg2000 dir] [out dir]
import json, sys, urllib.request, pathlib
base="https://hayro-assets.dev/jpeg2000"
root=pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else pathlib.Path.home()/"work/hayro/hayro-jpeg2000")
out=pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else pathlib.Path.home()/"tmp/jp2-corpus/hayro-assets")
for ns in ["serenity","openjpeg","custom"]:
    p=root/f"manifest_{ns}.json"
    items=json.loads(p.read_text())
    for it in items:
        path = it if isinstance(it,str) else (it.get("path") or it.get("file") or it.get("id"))
        dest=out/ns/path
        dest.parent.mkdir(parents=True,exist_ok=True)
        if dest.exists(): continue
        try:
            req=urllib.request.Request(f"{base}/{ns}/{path}",headers={"User-Agent":"inv-fetch/1.0"})
            dest.write_bytes(urllib.request.urlopen(req,timeout=60).read())
            print("ok",ns,path,dest.stat().st_size)
        except Exception as e:
            print("FAIL",ns,path,e)
