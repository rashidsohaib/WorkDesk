# WorkDesk Chrome v2.2

Fixes
* **Automatic mail check now works.** "Check for mail every N minutes" was skipped whenever WorkDesk believed a sync was still running, and that flag could stay set after a manual Refresh. The background check now syncs the Inbox and the open folder on schedule regardless, refreshes the list when something changed (without disturbing a selection or a search), and shows "N new messages in your Inbox". The page also no longer slows its timers when the window is in the background.
* **Theme colour with "Colour the header and left rail too":** text typed in the search box and the sync status text (Up to date, Downloading headers…) were white on a light surface and so invisible. Both now keep the normal text colour in every theme colour and in dark mode.
