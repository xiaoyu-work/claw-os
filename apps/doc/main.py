"""doc — Universal document reader for Claw OS.

Throw any file at it, get structured text back.
"""

import csv
import io
import json
import os


def _read_txt(path):
    with open(path, "r", encoding="utf-8") as f:
        text = f.read()
    return text


def _read_json(path):
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    return json.dumps(data, indent=2, ensure_ascii=False)


def _read_csv(path):
    with open(path, "r", encoding="utf-8", newline="") as f:
        reader = csv.DictReader(f)
        rows = list(reader)
    return json.dumps(rows, indent=2, ensure_ascii=False)


def _read_yaml(path):
    try:
        import yaml
    except ImportError:
        # yaml is in stdlib via pyyaml on most systems; fall back to raw text
        return _read_txt(path)
    with open(path, "r", encoding="utf-8") as f:
        data = yaml.safe_load(f)
    return json.dumps(data, indent=2, ensure_ascii=False)


def _read_pdf(path):
    try:
        import fitz  # pymupdf
    except ImportError:
        return None, {"error": "pymupdf is not installed", "hint": "cos pkg need python3-pymupdf"}
    doc = fitz.open(path)
    pages = []
    for page in doc:
        pages.append(page.get_text())
    doc.close()
    return "\n".join(pages), None


def _read_docx(path):
    try:
        import docx
    except ImportError:
        return None, {"error": "python-docx is not installed", "hint": "cos pkg need python3-docx"}
    doc = docx.Document(path)
    paragraphs = [p.text for p in doc.paragraphs]
    return "\n".join(paragraphs), None


def _read_xlsx(path):
    try:
        import openpyxl
    except ImportError:
        return None, {"error": "openpyxl is not installed", "hint": "cos pkg need python3-openpyxl"}
    wb = openpyxl.load_workbook(path, read_only=True, data_only=True)
    sheets = {}
    for name in wb.sheetnames:
        ws = wb[name]
        rows = []
        for row in ws.iter_rows(values_only=True):
            rows.append([str(c) if c is not None else "" for c in row])
        sheets[name] = rows
    wb.close()
    return json.dumps(sheets, indent=2, ensure_ascii=False), None


def _read_pptx(path):
    try:
        from pptx import Presentation
    except ImportError:
        return None, {"error": "python-pptx is not installed", "hint": "pip install python-pptx"}
    prs = Presentation(path)
    slides = []
    for i, slide in enumerate(prs.slides, 1):
        texts = []
        for shape in slide.shapes:
            if shape.has_text_frame:
                for para in shape.text_frame.paragraphs:
                    text = para.text.strip()
                    if text:
                        texts.append(text)
        notes = ""
        if slide.has_notes_slide and slide.notes_slide.notes_text_frame:
            notes = slide.notes_slide.notes_text_frame.text.strip()
        slide_text = f"--- Slide {i} ---\n" + "\n".join(texts)
        if notes:
            slide_text += f"\n\n[Notes] {notes}"
        slides.append(slide_text)
    return "\n\n".join(slides), None


def _ext(path):
    return os.path.splitext(path)[1].lower()


def _line_count(text):
    if not text:
        return 0
    return text.count("\n") + (0 if text.endswith("\n") else 1)


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def cmd_read(args):
    if not args:
        return {"error": "usage: cos doc read <path>"}
    path = args[0]
    if not os.path.isfile(path):
        return {"error": f"file not found: {path}"}

    ext = _ext(path)
    fmt = ext.lstrip(".") or "txt"

    # Formats that need external libs — may return an error dict
    if ext == ".pdf":
        content, err = _read_pdf(path)
        if err:
            return err
    elif ext == ".docx":
        content, err = _read_docx(path)
        if err:
            return err
    elif ext == ".xlsx":
        content, err = _read_xlsx(path)
        if err:
            return err
    elif ext == ".pptx":
        content, err = _read_pptx(path)
        if err:
            return err
    elif ext in (".yaml", ".yml"):
        content = _read_yaml(path)
    elif ext == ".json":
        try:
            content = _read_json(path)
        except (json.JSONDecodeError, UnicodeDecodeError) as e:
            return {"error": f"failed to parse JSON: {e}"}
    elif ext == ".csv":
        try:
            content = _read_csv(path)
        except (UnicodeDecodeError, csv.Error) as e:
            return {"error": f"failed to parse CSV: {e}"}
    elif ext in (".txt", ".md"):
        try:
            content = _read_txt(path)
        except UnicodeDecodeError:
            return {"error": f"unsupported format: {ext} (binary content)"}
    else:
        # Unknown extension — try reading as UTF-8 text
        try:
            content = _read_txt(path)
        except UnicodeDecodeError:
            return {"error": f"unsupported format: {ext}"}

    return {
        "path": path,
        "format": fmt,
        "content": content,
        "lines": _line_count(content),
    }


def cmd_info(args):
    if not args:
        return {"error": "usage: cos doc info <path>"}
    path = args[0]
    if not os.path.exists(path):
        return {"error": f"file not found: {path}"}

    ext = _ext(path)
    fmt = ext.lstrip(".") or "txt"
    size = os.path.getsize(path)

    # Determine readability: can we handle this format?
    readable = True
    if ext == ".pdf":
        try:
            import fitz  # noqa: F401
        except ImportError:
            readable = False
    elif ext == ".docx":
        try:
            import docx  # noqa: F401
        except ImportError:
            readable = False
    elif ext == ".xlsx":
        try:
            import openpyxl  # noqa: F401
        except ImportError:
            readable = False
    elif ext == ".pptx":
        try:
            from pptx import Presentation  # noqa: F401
        except ImportError:
            readable = False
    else:
        # For unknown extensions, probe if it looks like text
        if ext not in (".txt", ".md", ".json", ".csv", ".yaml", ".yml"):
            try:
                with open(path, "r", encoding="utf-8") as f:
                    f.read(512)
            except (UnicodeDecodeError, OSError):
                readable = False

    return {
        "path": path,
        "format": fmt,
        "size": size,
        "readable": readable,
    }


def cmd_convert(args):
    if not args:
        return {"error": "usage: cos doc convert <path> --to <format>"}

    path = args[0]
    if not os.path.isfile(path):
        return {"error": f"file not found: {path}"}

    # Parse --to <format>
    target_fmt = None
    for i, a in enumerate(args):
        if a == "--to" and i + 1 < len(args):
            target_fmt = args[i + 1].lstrip(".")
            break
    if not target_fmt:
        return {"error": "usage: cos doc convert <path> --to <format>"}

    ext = _ext(path)
    base = os.path.splitext(path)[0]
    output_path = f"{base}.{target_fmt}"

    # JSON -> CSV
    if ext == ".json" and target_fmt == "csv":
        try:
            with open(path, "r", encoding="utf-8") as f:
                data = json.load(f)
        except (json.JSONDecodeError, UnicodeDecodeError) as e:
            return {"error": f"failed to parse JSON: {e}"}
        if not isinstance(data, list) or not data or not isinstance(data[0], dict):
            return {"error": "JSON must be an array of objects for CSV conversion"}
        fieldnames = list(data[0].keys())
        buf = io.StringIO()
        writer = csv.DictWriter(buf, fieldnames=fieldnames)
        writer.writeheader()
        for row in data:
            writer.writerow(row)
        with open(output_path, "w", encoding="utf-8", newline="") as f:
            f.write(buf.getvalue())
        return {"input": path, "output": output_path, "format": "csv"}

    # CSV -> JSON
    if ext == ".csv" and target_fmt == "json":
        try:
            with open(path, "r", encoding="utf-8", newline="") as f:
                reader = csv.DictReader(f)
                rows = list(reader)
        except (UnicodeDecodeError, csv.Error) as e:
            return {"error": f"failed to parse CSV: {e}"}
        with open(output_path, "w", encoding="utf-8") as f:
            json.dump(rows, f, indent=2, ensure_ascii=False)
        return {"input": path, "output": output_path, "format": "json"}

    return {"error": "unsupported conversion"}


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def run(command, args):
    """Called by cos router."""
    commands = {
        "read": cmd_read,
        "info": cmd_info,
        "convert": cmd_convert,
    }
    handler = commands.get(command)
    if not handler:
        return {"error": f"unknown command: {command}"}
    return handler(args)
