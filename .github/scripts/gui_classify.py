"""Use the same changed-file policy as request resolution and final gating."""
import json
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
from gui_policy import required_from_files


def main():
    try:
        pages = json.load(sys.stdin)
        if not isinstance(pages, list) or any(not isinstance(page, list) for page in pages):
            raise ValueError('invalid paginated file evidence')
        rows = [row for page in pages for row in page]
        required = required_from_files(rows, int(sys.argv[1]))
        print('true' if required else 'false')
    except (ValueError, TypeError, IndexError):
        print('blocked')


if __name__ == '__main__':
    main()
