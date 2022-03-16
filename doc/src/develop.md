# Developing for Twizzler

The Twizzler project welcomes people to contribute to this open source
operating system.  We follow a branch and pull-request process, and
while it's fine to also work in your own fork, it's probably easier to
work within the main repo in your own branches.

# Branch Naming

Branch names should be short and descriptive, containing the user's
github or other short name, following by a dash and then a feature
name, e.g. gnn-icmp.  Names must not violate the Twizzler project's
Code of Conduct.  Please keep it classy.

# Submitting a Pull Request

All pull requests should be against the 'main' branch.  From time to
time there may be special exceptions but these must be coordinated
with the project owners, listed on the main github page.

# Example Workflow

In order to create this set of documentation the following steps were
carried out.

```
> git clone git@github.com:twizzler-operating-system/twizzler.git

Create and edit file in doc/src/develop.md

> git branch -b gnn-docs
> git add doc/src/develop.md
> git commit
> git push --set-upstream origin gnn-docs

The pull request was then submitted from the github page for the
Twizzer project.

Two reviewers were added at the time the PR was committed.
