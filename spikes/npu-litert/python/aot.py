"""AOT-compile the tflite pair for every Qualcomm and MediaTek SoC LiteRT 2.2.0 knows."""
import json, pathlib, shutil, sys

from ai_edge_litert.aot import aot_compile as aot_lib
from ai_edge_litert.aot.vendors.mediatek import target as mtk
from ai_edge_litert.aot.vendors.qualcomm import target as qnn

def targets():
    for soc in qnn.SocModel:
        if soc != qnn.SocModel.ALL:
            yield soc.value, qnn.Target(soc)
    for soc in mtk.SocModel:
        if soc != mtk.SocModel.ALL:
            yield soc.value, mtk.Target(soc)

def main(out: pathlib.Path) -> None:
    only = set(sys.argv[2:])
    report = {}
    for name, target in targets():
        if only and name not in only:
            continue
        report[name] = {}
        for graph in ("encoder", "decoder"):
            dest = out / "npu" / name
            dest.mkdir(parents=True, exist_ok=True)
            try:
                result = aot_lib.aot_compile(str(out / f"{graph}.tflite"), output_dir=str(dest / "_tmp"), target=target, keep_going=True)
                if result.failed_backends:
                    report[name][graph] = {"error": [str(e) for _, e in result.failed_backends]}
                    continue
                _, model = result.models_with_backend[0]
                shutil.copy(model.path, dest / f"{graph}.tflite")
                stats = model.partition_stats
                report[name][graph] = {
                    "bytes": (dest / f"{graph}.tflite").stat().st_size,
                    "subgraphs": [s.__dict__ for s in (stats.subgraph_stats if stats else [])],
                }
            except Exception as error:  # a spike records every failure and goes on
                report[name][graph] = {"error": repr(error)}
            finally:
                shutil.rmtree(dest / "_tmp", ignore_errors=True)
        print(name, json.dumps(report[name])[:300], flush=True)
    (out / "npu" / ("report.json" if not only else "report-partial.json")).write_text(json.dumps(report, indent=2))
    ok = [n for n, g in report.items() if all("bytes" in v for v in g.values())]
    print(f"compiled for {len(ok)} of {len(report)} SoCs: {' '.join(ok)}")

if __name__ == "__main__":
    main(pathlib.Path(sys.argv[1]))
