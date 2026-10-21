' Starts the SystemDeck server in the background (no console window).
' Output goes to data\server.log. Stop it with stop-server.bat.
Set fso = CreateObject("Scripting.FileSystemObject")
dir = fso.GetParentFolderName(WScript.ScriptFullName)
If Not fso.FolderExists(dir & "\data") Then fso.CreateFolder(dir & "\data")
cmd = "cmd /c """"" & dir & "\systemdeck-server.exe"" --data-dir """ & dir & "\data"" >> """ & dir & "\data\server.log"" 2>&1"""
CreateObject("WScript.Shell").Run cmd, 0, False
