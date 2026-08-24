#!/usr/bin/env seal
const fs = require("@std/fs")
const args = require("@std/args")
const process = require("@std/process")
const colors = require("@std/colors")

const REPO_URL = "https://github.com/luwu-community/luwu"

const parsed = args.parse("update_luwu.luau", "re-vendor a specific luwu directory into mluau"):simple(
    args.named("--branch", "specify upstream branch of luwu to clone (use this or --path)")
        :aliases("-b"):default("master"),
    args.named("--path", "local path to repository current branch for development purposes")
        :aliases("-p")
)

const branch = parsed:expect<<string>>("--branch")
local path = parsed:get<<string>>("--path")
if path and path:find("~") then
    path = string.gsub(path, "~", fs.path.home())
end
const use_local_path = path ~= nil

const important_directories_in_luwu = { "Ast", "CodeGen", "Common", "Bytecode", "Require", "VM", "Compiler", "Config" }

const project = fs.dir.project()
const vendored_path = project:join("luwu")
const temp_path = project:join("luau-temp")

const function find_relevant_directories(path: string)
    const relevant_dirs = fs.listdir(path, false, function(p)
        const child = assert(fs.path.child(p))
        if table.find(important_directories_in_luwu, child) then
            print(`found {child}`)
            return true
        end
        return false
    end)
    if #relevant_dirs == 0 then
        error(`Can't find any of the relevant dirs in '{path}'?`)
    end
    return relevant_dirs
end

const function replace_relevant_directories(relevant_directories: { string })
    fs.dir.try_remove(vendored_path)
    const vendored = fs.dir.ensure(vendored_path)
    for _, r in relevant_directories do
        const child = assert(fs.path.child(r))
        const directory_vendored_path = vendored:join(child)
        fs.copy(r, directory_vendored_path)
    end
end

const function replace_using_local_path()
    print(`Regenerating ./luwu directory from local path '{path}'...`)
    assert(path ~= nil, "--path must be specified here")
    if fs.is(path) ~= "Directory" then
        error(`update_luwu.luau: path provided '{path} is not a directory`)
    end
    const relevant_dirs = find_relevant_directories(path)
    replace_relevant_directories(relevant_dirs)
end

const function replace_using_git_clone()
    print("Regenerating ./luau directory from git clone...")
    fs.dir.try_remove(temp_path)
    process.run {
        program = "git",
        args = { "clone", "--depth", "1", "-b", branch, REPO_URL, temp_path},
        stdio = "Inherit"
    }:unwrap()
    const relevant_directories = find_relevant_directories(temp_path)
    replace_relevant_directories(relevant_directories)
    fs.dir.try_remove(temp_path)
end

if use_local_path then
    replace_using_local_path()
else
    replace_using_git_clone()
end

print(colors.green(`Done! Luwu source folders have been vendored to {vendored_path}!`))