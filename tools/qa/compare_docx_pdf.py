"""Local QA for text-only DOCX and its Word PDF; never used by the editor.

Requires pdfplumber. Output contains measurements, not source text or paths.
Automatic en-dash list labels are excluded from the reference text mapping.
"""
import argparse
import csv
import difflib
import json
import statistics
import xml.etree.ElementTree as ET
from pathlib import Path
from zipfile import ZipFile

import pdfplumber


def rows(path):
    return list(csv.DictReader(path.read_text(encoding="utf-8").splitlines()[1:], delimiter="\t"))


def verify(docx, pdf_path, render_dir):
    with ZipFile(docx) as archive:
        root = ET.fromstring(archive.read("word/document.xml"))
    namespace = root.tag.split("}")[0] + "}"
    source, locations = [], []
    for paragraph, node in enumerate(root.findall(f"{namespace}body/{namespace}p")):
        offset = 0
        for item in node.iter():
            if item.tag == namespace + "t":
                for char in item.text or "":
                    if not char.isspace():
                        source.append(char)
                        locations.append((paragraph, offset))
                    offset += len(char.encode("utf-16-le")) // 2
            elif item.tag in (namespace + "tab", namespace + "br", namespace + "cr"):
                offset += 1
    reference, positions = [], []
    with pdfplumber.open(pdf_path) as pdf:
        page_count = len(pdf.pages)
        for page_number, page in enumerate(pdf.pages, 1):
            lines = []
            for char in page.chars:
                baseline = page.height - char["matrix"][5]
                line = next((line for line in lines if abs(line[0] - baseline) < 0.2), None)
                if line is None:
                    line = (baseline, [])
                    lines.append(line)
                line[1].append(char)
            for line_number, (baseline, chars) in enumerate(sorted(lines, key=lambda line: line[0])):
                if chars and chars[0]["text"] == "–":
                    chars = chars[1:]
                for char in chars:
                    for letter in char["text"]:
                        if not letter.isspace():
                            reference.append(letter)
                            positions.append((page_number, line_number, char["x0"], baseline))
    matcher = difflib.SequenceMatcher(a="".join(source), b="".join(reference), autojunk=False)
    mapping = {}
    text_errors = 0
    for tag, start, end, ref_start, ref_end in matcher.get_opcodes():
        if tag != "equal":
            text_errors += max(end - start, ref_end - ref_start)
            continue
        for index in range(end - start):
            mapping[locations[start + index]] = positions[ref_start + index]
    glyphs = rows(render_dir / "glyphs.tsv")
    lines = rows(render_dir / "layout.tsv")
    seen, x_errors, y_errors = set(), [], []
    extra = page_errors = 0
    for glyph in glyphs:
        key = (int(glyph["paragraph"]) - 1, int(glyph["offset"]))
        if key not in mapping or key in seen:
            extra += 1
            continue
        seen.add(key)
        page, _, x, baseline = mapping[key]
        page_errors += page != int(glyph["page"])
        x_errors.append(abs(float(glyph["x"]) * 0.75 - x))
        y_errors.append(abs(float(glyph["baseline"]) * 0.75 - baseline))
    line_errors = visible_lines = 0
    reference_owners = {}
    for native_line, line in enumerate(lines):
        paragraph = int(line["paragraph"]) - 1
        start = int(line["start"])
        references = {
            mapping[(paragraph, offset)][:2]
            for offset in range(start, start + int(line["length"]))
            if (paragraph, offset) in mapping
        }
        if references:
            visible_lines += 1
            line_errors += len(references) != 1 or next(iter(references))[0] != int(line["page"])
            for reference_line in references:
                reference_owners.setdefault(reference_line, set()).add(native_line)
    line_errors += sum(len(owners) != 1 for owners in reference_owners.values())
    native_pages = max((int(line["page"]) for line in lines), default=0)
    ordered = sorted(x_errors)
    return {
        "reference_pages": page_count,
        "rendered_pages": native_pages,
        "visible_lines": visible_lines,
        "reference_visible_lines": len({position[:2] for position in mapping.values()}),
        "compared_characters": len(x_errors),
        "text_errors": text_errors,
        "missing_characters": len(set(locations) - seen),
        "extra_characters": extra,
        "character_page_errors": page_errors,
        "line_break_errors": line_errors,
        "character_x_median_pt": statistics.median(ordered) if ordered else None,
        "character_x_p95_pt": ordered[int(0.95 * len(ordered))] if ordered else None,
        "character_x_max_pt": max(ordered, default=None),
        "baseline_median_pt": statistics.median(y_errors) if y_errors else None,
        "baseline_max_pt": max(y_errors, default=None),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--docx", type=Path, required=True)
    parser.add_argument("--pdf", type=Path, required=True)
    parser.add_argument("--render-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--max-x-pt", type=float, default=0.5)
    parser.add_argument("--max-baseline-pt", type=float, default=0.2)
    args = parser.parse_args()
    result = verify(args.docx, args.pdf, args.render_dir)
    result["passed"] = (
        result["reference_pages"] == result["rendered_pages"]
        and result["visible_lines"] == result["reference_visible_lines"]
        and all(result[key] == 0 for key in (
            "text_errors", "missing_characters", "extra_characters", "character_page_errors", "line_break_errors"
        ))
        and result["character_x_max_pt"] is not None
        and result["character_x_max_pt"] <= args.max_x_pt
        and result["baseline_max_pt"] <= args.max_baseline_pt
    )
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, ensure_ascii=False, indent=2))
    raise SystemExit(0 if result["passed"] else 1)


if __name__ == "__main__":
    main()
