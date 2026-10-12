# Phase 7 field test (for Austin)

The last gate is a real Square account, used with the owner's permission, on one Mac and one PC for a week. CI can't do these checks: it can't reboot, sign in at a real login screen, or see Square's own screens. Austin signs off at the end.

**Before you start**
- [ ] `SQUARE_APP_SETUP.md` is done: the production Square application exists, and its id is a GitHub secret (`SQUARE_APP_ID`).
- [ ] A release candidate was built by **Actions → release** and every install test passed.
- [ ] The seller has agreed in writing to a test on their account, and knows the app writes one "Last sold" label on their items and nothing else.

## Day 0: install and connect (Mac and PC)

- [ ] Download from the real page (tightline.systems/deadstock) on the seller's computer, not from a USB stick.
- [ ] Mac: the "downloaded from the internet" prompt appears once, and Open works. Windows: note exactly what SmartScreen shows (photo).
- [ ] **Connect Square** opens Square's page in the browser. Log in, approve, and the app says "Connected to *business*". Take a photo of Square's approval screen.
- [ ] If production refuses the `localhost` redirect, stop and tell Claude Code (`SQUARE_FACTS.md`, Redirect URL).
- [ ] Setup: 90 days, the right locations, Labels on, 5:00 a.m., Check in the background on. Press **Start**.
- [ ] Note how long the first sync takes and how many items it reports.
- [ ] Mac: macOS shows "Dead Stock Finder added a background item". System Settings → General → Login Items lists Dead Stock Finder.
- [ ] Windows: Task Scheduler (Library root) shows "Dead Stock Finder check (*user*)" with two triggers.

## Day 0: labels in Square

- [ ] In the Square Dashboard, open three items: one slow, one that sold recently, one with several variations. Take screenshots of the "Last sold" label on each item page, and in the variation panel for the multi-variation item.
- [ ] In Square's Item Library, try searching for "SLOW". Note whether it finds the slow items (search by custom attribute).
- [ ] Open the same items in the **Square Point of Sale app** on a phone or terminal. Take screenshots. The docs say custom attributes don't show there; record what you actually see.
- [ ] Spot-check five items against the seller's own memory or sales report: is the "last sold" date right?

## Days 1 to 7: background checks

- [ ] Each morning, open the app (or just read the status line): "last check today 5:0x a.m.".
- [ ] **Real reboot, Mac:** restart, sign in, wait 2 minutes. The status line shows a check at sign-in time.
- [ ] **Real reboot, PC:** the same.
- [ ] Let one machine sleep through 5:00 a.m. once. On wake, a check runs within a few minutes.
- [ ] Sell one slow item in Square POS. After the next check, its label reads "Last sold *today*".
- [ ] No window appears and nothing steals focus during background checks.

## End of week

- [ ] **Help → Save diagnostics** on both machines. Keep the files; check that they show no item names, amounts or tokens.
- [ ] **Settings → Remove all labels from Square.** In the Dashboard, the label is gone from the three items.
- [ ] **Settings → Disconnect Square**, then in the Square Dashboard: Settings → App integrations → Disconnect App.
- [ ] Uninstall on both machines. Windows: Task Scheduler no longer lists the task. Mac: Login Items no longer lists the app.

Write the results, photos and screenshots into `TEST_REPORT.md` (Field test section) and sign off.
