# CrashTest Example

If you run `CrashTest.exe` it will immediately crash. You may also open `CrashTest.dmp`. When you debug with Visual Studio it will attempt to invoke:

`fts_pdbsrc extract_one --pdb-uuid bd0fb37c-9a3b-48c7-b75f-cc4c08c5fd81 --file CrashTest\CrashTest.cpp --out C:\Users\Forrest\AppData\Local/fts_pdbsrc/temp/CrashTest/CrashTest.cpp`

If you followed the instructions to install `fts_pdbsrc.exe` and `fts_pdbsrc_service.exe` into your path this command should succeed and Visual Studio should automatically open the file.