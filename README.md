# fts_pdbsrc

This creates a cmdline tool `fts_pdbsrc.exe` which is used to both embed and extract source code into `.pdb` files on the Windows platform. The source embed operation is performed such that Microsoft Visual Studio is able to automatically extract and open source files for debugging.

For a technical deep dive please refer to the [blog post](https://www.forrestthewoods.com/blog/embedding-source-code-in-pdbs-with-rust).

This project is dual-licensed under MIT or the UNLICENSE.

# Why Make This?

Debugging a project requires three things:

1. Compiled Binary or Crash Dump
1. Debug Symbols
1. Source code

It's easy to distribute #1 and #2. Just ship an `.exe` and `.pdb`. However #3 might be thousands of files in a complex directory structure. Wouldn't it be nice if source was just included in the `.pdb`? I think so. Especially for open source projects.

That's what this tool does.

`fts_pdbsrc.exe` is a cmdline tool that can both embed and extract source into/from `.pdb` files. `fts_pdbsrc_service.exe` is a tool that runs as a Windows service so `fts_pdbsrc.exe` can find matching symbols. Ideally Microsoft will update Visual Studio such that `fts_pdbsrc_service` is not necessary. See blog post for details.

# How to Use the Tool

To embed:

1. Run `fts_pdbsrc embed --pdb c:/path/to/foo.pdb --roots c:/path/to/ProjectRoot --encrypt-mode Plaintext`
    - Encrypt with rng key: `--encrypt-mode EncryptFromRngKey`
    - Encrypt key explicit key: `--encrypt-mode EncryptWithKey(0124567890124567890124567890124567890124567890124567890124567890)`

To extract:

1. Install `fts_pdbsrc.exe` and `fts_pdbsrc_service.exe` into your path
1. Add `.pdb` search directories to `fts_pdbsrc_service_config.json`
1. (Optional) Add decryption keys to `fts_pdbsrc_config.json`
1. Run `fts_pdbsrc.exe install_service` once
    a. To uninstall: `fts_pdbsrc.exe uninstall_service`
1. Debug with Visual Studio!
