# Gramcast privacy policy

Last updated: 22/11/2026

Gramcast is a personal tool. One person, its owner, uses it to schedule their own videos for posting to their own YouTube channel and Instagram account. It is not offered to the public and has no other users.

## What Gramcast stores

- Video files the owner uploads, in the owner's own Cloudflare R2 storage.
- Post details the owner enters (caption, title, scheduled time, status), in the owner's own Cloudflare D1 database.
- Sign-in tokens for the owner's YouTube channel and Instagram account, so Gramcast can post on the owner's behalf. These are stored only in the owner's own Cloudflare account.

## How the data is used

Gramcast uses this data only to upload and publish the owner's videos to the owner's own accounts, at the times the owner chooses. It does not sell, share or transfer the data to anyone else, and it does not use it for advertising or analytics.

## Google user data

Gramcast asks for the YouTube upload permission (`youtube.upload`) so it can upload the owner's videos to the owner's channel. Data received from Google APIs is used only for that purpose.

## Instagram user data

Gramcast asks for permission to read basic account details and publish content (`instagram_business_basic` and `instagram_business_content_publish`) so it can publish the owner's Reels to the owner's account.

## Removing access

The owner can revoke Gramcast's access at any time in their Google account permissions and in their Instagram account's app settings, and can delete the stored data by deleting the Gramcast Worker, database and storage bucket in Cloudflare.

## Contact

Questions can be raised as an issue at https://github.com/bconolly/bc-gramcast/issues.
