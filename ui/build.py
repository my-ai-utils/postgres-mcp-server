import random
import string
import argparse


def replace_wasm_with_random_string(file_path):
    def generate_random_string(length=16):
        characters = string.ascii_letters + string.digits
        return ''.join(random.choice(characters) for _ in range(length))

    with open(file_path, 'r') as file:
        content = file.read()

    updated_content = content.replace('.wasm', f'.wasm?id={generate_random_string()}')
    updated_content = updated_content.replace('.js', f'.js?id={generate_random_string()}')
    updated_content = updated_content.replace('.css', f'.css?id={generate_random_string()}')

    with open(file_path, 'w') as file:
        file.write(updated_content)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Cache-bust static assets in index.html")
    parser.add_argument("file_path", help="Path to the index.html file")
    args = parser.parse_args()
    replace_wasm_with_random_string(args.file_path)
