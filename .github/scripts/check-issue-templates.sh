#!/usr/bin/env bash
# Validates the GitHub issue forms in .github/ISSUE_TEMPLATE. Every form must
# parse as YAML and declare a name, a description, a non-empty list of labels,
# and a body whose fields use a known type, carry a unique id, and have a
# label. config.yml must set blank_issues_enabled. Syntax reference:
# https://docs.github.com/en/communities/using-templates-to-encourage-useful-issues-and-pull-requests/syntax-for-issue-forms
set -euo pipefail

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
template_dir="$workspace_root/.github/ISSUE_TEMPLATE"

if [ ! -d "$template_dir" ]; then
  echo "Error: issue template directory not found at $template_dir" >&2
  exit 1
fi

python3 - "$template_dir" <<'PY'
import glob
import os
import sys

import yaml

VALID_TYPES = {"markdown", "textarea", "input", "dropdown", "checkboxes"}
OPTION_TYPES = {"dropdown", "checkboxes"}

template_dir = sys.argv[1]
errors = []


def fail(name, message):
    errors.append(f"{name}: {message}")


def check_form(name, data):
    for key in ("name", "description"):
        if not isinstance(data.get(key), str) or not data[key].strip():
            fail(name, f"missing or empty '{key}'")

    labels = data.get("labels")
    if (
        not isinstance(labels, list)
        or not labels
        or not all(isinstance(label, str) and label.strip() for label in labels)
    ):
        fail(name, "'labels' must be a non-empty list of label names")

    body = data.get("body")
    if not isinstance(body, list) or not body:
        fail(name, "'body' must be a non-empty list of fields")
        return

    ids = set()
    for index, field in enumerate(body, start=1):
        where = f"body[{index}]"
        if not isinstance(field, dict):
            fail(name, f"{where} must be a mapping")
            continue

        kind = field.get("type")
        if kind not in VALID_TYPES:
            fail(name, f"{where} has unknown type {kind!r}; expected one of {sorted(VALID_TYPES)}")
            continue
        if kind == "markdown":
            continue

        field_id = field.get("id")
        if not isinstance(field_id, str) or not field_id.strip():
            fail(name, f"{where} ({kind}) needs a non-empty 'id'")
        elif field_id in ids:
            fail(name, f"{where} reuses id '{field_id}'; ids must be unique per form")
        else:
            ids.add(field_id)

        attributes = field.get("attributes")
        if not isinstance(attributes, dict):
            fail(name, f"{where} ({field_id}) needs an 'attributes' mapping")
            continue
        if not isinstance(attributes.get("label"), str) or not attributes["label"].strip():
            fail(name, f"{where} ({field_id}) needs a non-empty attributes.label")

        if kind in OPTION_TYPES:
            options = attributes.get("options")
            if not isinstance(options, list) or not options:
                fail(name, f"{where} ({field_id}) needs a non-empty attributes.options list")

        validations = field.get("validations")
        if validations is not None and (
            not isinstance(validations, dict)
            or not isinstance(validations.get("required", False), bool)
        ):
            fail(name, f"{where} ({field_id}) has invalid validations; 'required' must be true or false")


def check_config(name, data):
    if not isinstance(data, dict) or not isinstance(data.get("blank_issues_enabled"), bool):
        fail(name, "config.yml must set 'blank_issues_enabled' to true or false")


paths = sorted(glob.glob(os.path.join(template_dir, "*.yml")))
if not paths:
    fail(template_dir, "no issue templates (*.yml) found")

for path in paths:
    name = os.path.basename(path)
    try:
        with open(path, encoding="utf-8") as handle:
            data = yaml.safe_load(handle)
    except yaml.YAMLError as exc:
        fail(name, f"invalid YAML: {exc}")
        continue

    if not isinstance(data, dict):
        fail(name, "must be a YAML mapping")
        continue

    if name == "config.yml":
        check_config(name, data)
    else:
        check_form(name, data)

if errors:
    print("Issue template validation failed:")
    for error in errors:
        print(f"  - {error}")
    sys.exit(1)

print(f"OK: {len(paths)} issue template file(s) have valid metadata.")
PY
